import React, { useEffect } from "react";
import { act, cleanup, render, waitFor } from "@testing-library/react";
import { useQueryClient, type QueryClient } from "@tanstack/react-query";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { api } from "@/lib/api/client";
import type { ApiResponse, UserInfo } from "@/lib/api/types";
import { AuthProvider, useAuth } from "@/lib/auth-context";
import { storeSession } from "@/lib/auth-storage";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((done) => { resolve = done; });
  return { promise, resolve };
}

const token = `header.${btoa(JSON.stringify({ exp: 9999999999 }))}.signature`;
let auth: ReturnType<typeof useAuth>;
let client: QueryClient;

function Probe() {
  const value = useAuth();
  const queryClient = useQueryClient();
  useEffect(() => {
    auth = value;
    client = queryClient;
  }, [value, queryClient]);
  return <span>{value.user?.nickname || "no user"}</span>;
}

async function mount() {
  render(<AuthProvider><Probe /></AuthProvider>);
  await waitFor(() => expect(auth.hydrated).toBe(true));
}

beforeEach(() => {
  localStorage.clear();
  vi.spyOn(api, "getUserInfo").mockResolvedValue({ status: 0 });
  vi.spyOn(api, "login").mockResolvedValue({ status: 0, data: { token } });
  vi.spyOn(api, "register").mockResolvedValue({
    status: 0, data: { token, api_token: "new-api-token" },
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  localStorage.clear();
});

describe("session cache isolation", () => {
  it.each(["logout", "login", "register", "expired"] as const)(
    "%s clears all caches, cancels queries and retires the old client",
    async (action) => {
      storeSession(token, "alice");
      await mount();
      const previous = client;
      const keys = [
        ["records", "detail"], ["detail", "bangumi", "1"], ["episodes", 1],
        ["tokens"], ["tokens", "permissions"], ["settings", "auto-cleanup"],
        ["logs", "recording"], ["logs", "system"],
      ];
      keys.forEach((key) => previous.setQueryData(key, "alice's data"));
      previous.getMutationCache().build(previous, {}).state.data = "secret";
      const pending = deferred<string>();
      let signal!: AbortSignal;
      const query = previous.fetchQuery({
        queryKey: ["pending"],
        queryFn: (context) => { signal = context.signal; return pending.promise; },
      }).catch(() => undefined);

      await act(async () => {
        if (action === "expired")
          window.dispatchEvent(new Event("bangumi-recorder:session-expired"));
        else if (action === "logout") auth.logout();
        else await auth[action]("bob", "password");
      });

      expect(signal.aborted).toBe(true);
      expect(previous.getQueryCache().getAll()).toHaveLength(0);
      expect(previous.getMutationCache().getAll()).toHaveLength(0);
      expect(client).not.toBe(previous);
      // A late optimistic rollback must only touch the retired cache.
      previous.setQueryData(["settings", "auto-cleanup"], "alice's rollback");
      pending.resolve("alice's late response");
      await query;
      keys.forEach((key) => expect(client.getQueryData(key)).toBeUndefined());
      expect(client.getQueryData(["pending"])).toBeUndefined();
    },
  );

  it("ignores refresh responses and callbacks from a previous session, even with the same token", async () => {
    const old = deferred<ApiResponse<UserInfo>>();
    vi.mocked(api.getUserInfo).mockReturnValueOnce(old.promise);
    storeSession(token, "alice");
    await mount();
    await waitFor(() => expect(api.getUserInfo).toHaveBeenCalledOnce());
    const oldAuth = auth;
    const signal = vi.mocked(api.getUserInfo).mock.calls[0]?.[0];
    await act(async () => { await auth.login("bob", "password"); });
    expect(signal?.aborted).toBe(true);
    await act(async () => {
      old.resolve({ status: 0, data: { id: 1, nickname: "alice" } as UserInfo });
      await old.promise;
      await oldAuth.refreshUser();
      oldAuth.updateLocalUser({ nickname: "alice" });
      oldAuth.logout();
    });
    expect(auth.username).toBe("bob");
    expect(auth.user).toBeNull();
    expect(api.getUserInfo).toHaveBeenCalledTimes(2);
  });

  it("only applies the newest refresh within a session", async () => {
    storeSession(token, "alice");
    await mount();
    const older = deferred<ApiResponse<UserInfo>>();
    vi.mocked(api.getUserInfo).mockReturnValueOnce(older.promise)
      .mockResolvedValueOnce({ status: 0, data: { id: 1, nickname: "new" } as UserInfo });
    await act(async () => {
      const first = auth.refreshUser();
      await auth.refreshUser();
      older.resolve({ status: 0, data: { id: 1, nickname: "old" } as UserInfo });
      await first;
    });
    expect(auth.user?.nickname).toBe("new");
  });

  it("does not restore a pending login after logout", async () => {
    await mount();
    const response = deferred<Awaited<ReturnType<typeof api.login>>>();
    vi.mocked(api.login).mockReturnValueOnce(response.promise);
    await act(async () => {
      const login = auth.login("alice", "password");
      auth.logout();
      response.resolve({ status: 0, data: { token } });
      expect(await login).toEqual({ ok: false });
    });
    expect(auth.token).toBeNull();
  });

  it("keeps the current session and cache when login fails", async () => {
    storeSession(token, "alice");
    await mount();
    const previous = client;
    previous.setQueryData(["tokens"], "alice's tokens");
    vi.mocked(api.login).mockResolvedValueOnce({ status: -1, message: "invalid" });
    await act(async () => {
      expect(await auth.login("bob", "wrong")).toEqual({ ok: false, message: "invalid" });
    });
    expect(auth.username).toBe("alice");
    expect(client).toBe(previous);
    expect(client.getQueryData(["tokens"])).toBe("alice's tokens");
  });

  it("does not restore user data when a refresh completes after logout", async () => {
    const response = deferred<ApiResponse<UserInfo>>();
    vi.mocked(api.getUserInfo).mockReturnValueOnce(response.promise);
    storeSession(token, "alice");
    await mount();
    await waitFor(() => expect(api.getUserInfo).toHaveBeenCalledOnce());
    await act(async () => {
      auth.logout();
      response.resolve({ status: 0, data: { id: 1, nickname: "alice" } as UserInfo });
      await response.promise;
    });
    expect(auth.token).toBeNull();
    expect(auth.user).toBeNull();
  });
});
