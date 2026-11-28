import { createContext, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from "react";
import { api, ApiError, type ApiUser } from "./api";

/**
 * Real auth state, backed by the api-gateway session cookie. Wraps the whole app;
 * `useAuth()` anywhere gives the current user (or null) plus login/signup/logout.
 *
 * `loading` is true only during the initial `/me` probe on page load, so route
 * guards can wait instead of flashing a redirect.
 */
type AuthContextValue = {
  user: ApiUser | null;
  /** Connected SSO providers ("google", …) from /me — login-method factoids in settings. */
  ssoProviders: string[];
  loading: boolean;
  login: (email: string, password: string) => Promise<ApiUser>;
  /** `invitation` (code or token) redeems an invite during signup — see api.signup. */
  signup: (email: string, password: string, displayName?: string, invitation?: string) => Promise<ApiUser>;
  logout: () => Promise<void>;
  /** Re-fetch `/me` (e.g. after changing email so `email_verified` refreshes). */
  refresh: () => Promise<void>;
};

const AuthCtx = createContext<AuthContextValue>({
  user: null,
  ssoProviders: [],
  loading: true,
  login: async () => {
    throw new Error("AuthProvider missing");
  },
  signup: async () => {
    throw new Error("AuthProvider missing");
  },
  logout: async () => {},
  refresh: async () => {},
});

const signedOut = { user: null, ssoProviders: [] as string[] };

export function AuthProvider({ children }: { children: ReactNode }) {
  const [session, setSession] = useState<Pick<AuthContextValue, "user" | "ssoProviders">>(signedOut);
  const [loading, setLoading] = useState(true);
  const sessionRead = useRef<AbortController | null>(null);

  const cancelSessionRead = useCallback(() => {
    sessionRead.current?.abort();
    sessionRead.current = null;
  }, []);

  const refresh = useCallback(async () => {
    cancelSessionRead();
    const controller = new AbortController();
    sessionRead.current = controller;
    try {
      const result = await api.me(controller.signal);
      if (sessionRead.current === controller) {
        setSession({ user: result.user, ssoProviders: result.sso_providers });
      }
    } catch (error) {
      if (sessionRead.current === controller && error instanceof ApiError && error.status === 401) {
        setSession(signedOut);
      }
    } finally {
      if (sessionRead.current === controller) {
        sessionRead.current = null;
        setLoading(false);
      }
    }
  }, [cancelSessionRead]);

  useEffect(() => {
    void refresh();
    return cancelSessionRead;
  }, [cancelSessionRead, refresh]);

  const signedIn = useCallback((user: ApiUser) => {
    setSession({ user, ssoProviders: [] });
    setLoading(false);
    // Login and signup return identity immediately. Fetch avatar and SSO details separately.
    void refresh();
  }, [refresh]);

  const login = useCallback(async (email: string, password: string) => {
    const { user } = await api.login(email, password);
    signedIn(user);
    return user;
  }, [signedIn]);

  const signup = useCallback(async (email: string, password: string, displayName?: string, invitation?: string) => {
    const { user } = await api.signup(email, password, displayName, invitation);
    signedIn(user);
    return user;
  }, [signedIn]);

  const logout = useCallback(async () => {
    cancelSessionRead();
    try {
      await api.logout();
    } finally {
      cancelSessionRead();
      setSession(signedOut);
      setLoading(false);
    }
  }, [cancelSessionRead]);

  return <AuthCtx.Provider value={{ ...session, loading, login, signup, logout, refresh }}>{children}</AuthCtx.Provider>;
}

export const useAuth = () => useContext(AuthCtx);
