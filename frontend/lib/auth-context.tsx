"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { api, cancelSessionRequests } from "@/lib/api/client";
import type { UserInfo } from "@/lib/api/types";
import {
  clearStoredSession,
  getStoredToken,
  getStoredUsername,
  isTokenExpired,
  storeSession,
} from "@/lib/auth-storage";

interface AuthContextValue {
  hydrated: boolean;
  token: string | null;
  username: string | null;
  user: UserInfo | null;
  login: (
    username: string,
    password: string,
  ) => Promise<{ ok: boolean; message?: string }>;
  register: (
    username: string,
    password: string,
    registerToken?: string,
  ) => Promise<{ ok: boolean; message?: string }>;
  logout: () => void;
  refreshUser: () => Promise<void>;
  updateLocalUser: (patch: Partial<UserInfo>) => void;
}

const AuthContext = createContext<AuthContextValue | null>(null);

function createQueryClient() {
  return new QueryClient({
    defaultOptions: {
      queries: { staleTime: 30_000, retry: 1, refetchOnWindowFocus: false },
      mutations: { retry: 0 },
    },
  });
}

export function AuthProvider({ children }: { children: React.ReactNode }) {
  const [hydrated, setHydrated] = useState(false);
  const [token, setToken] = useState<string | null>(null);
  const [username, setUsername] = useState<string | null>(null);
  const [user, setUser] = useState<UserInfo | null>(null);
  const [queryClient, setQueryClient] = useState(createQueryClient);
  const [sessionKey, setSessionKey] = useState(0);
  const currentClient = useRef(queryClient);
  const sessionVersion = useRef(0);
  const authAttempt = useRef(0);
  const userRequest = useRef<AbortController | null>(null);

  const resetSession = useCallback(() => {
    sessionVersion.current += 1;
    setSessionKey(sessionVersion.current);
    authAttempt.current += 1;
    userRequest.current?.abort();
    cancelSessionRequests();
    void currentClient.current.cancelQueries();
    currentClient.current.clear();
    // Old mutation callbacks retain the retired client, never the new cache.
    currentClient.current = createQueryClient();
    setQueryClient(currentClient.current);
    setUser(null);
  }, []);

  const logout = useCallback(() => {
    if (sessionKey !== sessionVersion.current) return;
    resetSession();
    clearStoredSession();
    setToken(null);
    setUsername(null);
    setUser(null);
  }, [resetSession, sessionKey]);

  const refreshUser = useCallback(async () => {
    if (
      !token || token !== getStoredToken() ||
      sessionKey !== sessionVersion.current
    )
      return;
    userRequest.current?.abort();
    const controller = new AbortController();
    userRequest.current = controller;
    try {
      const response = await api.getUserInfo(controller.signal);
      if (
        !controller.signal.aborted &&
        sessionKey === sessionVersion.current &&
        token === getStoredToken() &&
        response.status === 0 && response.data?.id
      )
        setUser(response.data);
    } catch (error) {
      if (!controller.signal.aborted) throw error;
    }
  }, [token, sessionKey]);

  useEffect(() => {
    queueMicrotask(() => {
      const storedToken = getStoredToken();
      if (storedToken && !isTokenExpired(storedToken)) {
        setToken(storedToken);
        setUsername(getStoredUsername());
      } else if (storedToken) clearStoredSession();
      setHydrated(true);
    });
  }, []);

  useEffect(() => {
    if (!token) return;
    // Background refresh failures leave the session intact; explicit refreshes reject.
    queueMicrotask(() => void refreshUser().catch(() => {}));
    return () => userRequest.current?.abort();
  }, [token, refreshUser]);

  useEffect(() => {
    window.addEventListener("bangumi-recorder:session-expired", logout);
    return () =>
      window.removeEventListener("bangumi-recorder:session-expired", logout);
  }, [logout]);

  const login = useCallback(async (name: string, password: string) => {
    const attempt = ++authAttempt.current;
    const response = await api.login(name, password);
    if (attempt !== authAttempt.current) return { ok: false };
    if (response.status !== 0 || !response.data?.token)
      return { ok: false, message: response.message };
    resetSession();
    storeSession(response.data.token, name);
    setToken(response.data.token);
    setUsername(name);
    return { ok: true };
  }, [resetSession]);

  const register = useCallback(
    async (name: string, password: string, registerToken?: string) => {
      const attempt = ++authAttempt.current;
      const response = await api.register(name, password, registerToken);
      if (attempt !== authAttempt.current) return { ok: false };
      if (response.status !== 0 || !response.data?.token)
        return { ok: false, message: response.message };
      resetSession();
      storeSession(response.data.token, name);
      setToken(response.data.token);
      setUsername(name);
      return { ok: true };
    },
    [resetSession],
  );

  const value = useMemo<AuthContextValue>(
    () => ({
      hydrated,
      token,
      username,
      user,
      login,
      register,
      logout,
      refreshUser,
      updateLocalUser: (patch) => {
        if (sessionKey === sessionVersion.current && token === getStoredToken())
          setUser((current) => (current ? { ...current, ...patch } : current));
      },
    }),
    [hydrated, token, username, user, login, register, logout, refreshUser, sessionKey],
  );
  return (
    <AuthContext.Provider value={value}>
      <QueryClientProvider client={queryClient} key={sessionKey}>
        {children}
      </QueryClientProvider>
    </AuthContext.Provider>
  );
}

export function useAuth() {
  const context = useContext(AuthContext);
  if (!context) throw new Error("useAuth must be used inside AuthProvider");
  return context;
}
