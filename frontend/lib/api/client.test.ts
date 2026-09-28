import { afterEach, expect, it, vi } from "vitest";
import { api, cancelSessionRequests } from "@/lib/api/client";
import { getStoredToken, storeSession } from "@/lib/auth-storage";

afterEach(() => {
  cancelSessionRequests();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  localStorage.clear();
});

it.each([200, 401])("aborts session requests and rejects late %s responses", async (status) => {
  storeSession("alice-token", "alice");
  let resolve!: (response: Response) => void;
  const fetchMock = vi.fn<typeof fetch>(() =>
    new Promise<Response>((done) => { resolve = done; }),
  );
  vi.stubGlobal("fetch", fetchMock);
  const expired = vi.fn();
  window.addEventListener("bangumi-recorder:session-expired", expired);
  try {
    const pending = api.listTokens();
    const rejected = expect(pending).rejects.toThrow();
    cancelSessionRequests();
    storeSession("bob-token", "bob");
    expect(fetchMock.mock.calls[0]?.[1]?.signal?.aborted).toBe(true);
    resolve(new Response(JSON.stringify({ status: 0, data: ["alice-secret"] }), { status }));
    await rejected;
    expect(getStoredToken()).toBe("bob-token");
    expect(expired).not.toHaveBeenCalled();
  } finally {
    window.removeEventListener("bangumi-recorder:session-expired", expired);
  }
});

it("does not send a request with an already aborted caller signal", async () => {
  const fetchMock = vi.fn();
  vi.stubGlobal("fetch", fetchMock);
  const controller = new AbortController();
  controller.abort();
  await expect(api.getUserInfo(controller.signal)).rejects.toThrow();
  expect(fetchMock).not.toHaveBeenCalled();
});

it("still expires the current session on 401", async () => {
  storeSession("alice-token", "alice");
  vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("{}", { status: 401 })));
  const expired = vi.fn();
  window.addEventListener("bangumi-recorder:session-expired", expired);
  try {
    await api.listTokens();
    expect(getStoredToken()).toBeNull();
    expect(expired).toHaveBeenCalledOnce();
  } finally {
    window.removeEventListener("bangumi-recorder:session-expired", expired);
  }
});
