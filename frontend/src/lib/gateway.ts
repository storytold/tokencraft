import { API_BASE_URL, ApiError } from "./api";

export const demoEnabled = import.meta.env.VITE_TOKENCRAFT_DEMO === "true";

export type ProjectScope = { account_id: string; project_id: string };
export type Provider = string;

export function isProviderId(value: unknown): value is Provider {
  return typeof value === "string" && /^[a-z][a-z0-9_-]{0,63}$/.test(value);
}

export function withProject(path: string, projectId?: string, accountId?: string) {
  const params = new URLSearchParams();
  if (projectId) params.set("project_id", projectId);
  if (accountId) params.set("account_id", accountId);
  return params.size ? `${path}?${params}` : path;
}

export async function gatewayResponse(path: string, init?: RequestInit): Promise<Response> {
  let response: Response;
  try {
    response = await fetch(`${API_BASE_URL}${path}`, { credentials: "include", ...init });
  } catch (error) {
    if (init?.signal?.aborted) throw error;
    throw new ApiError(0, "network", "The gateway is unreachable. Check that the local server is running.");
  }
  if (response.status === 304 && new Headers(init?.headers).has("If-None-Match")) return response;
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as { error?: { message?: string; code?: string } } | null;
    if (response.status === 404 && body === null) {
      throw new ApiError(404, "endpoint_unavailable", "This endpoint is unavailable on the connected gateway.");
    }
    throw new ApiError(response.status, body?.error?.code ?? "request_failed", body?.error?.message ?? `Request failed (${response.status}).`);
  }
  return response;
}

export function isGatewayEndpointUnavailable(error: unknown) {
  return error instanceof ApiError && error.code === "endpoint_unavailable";
}

export async function gatewayRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await gatewayResponse(path, init);
  return response.status === 204 ? undefined as T : response.json() as Promise<T>;
}

export function errorMessage(error: unknown) {
  return error instanceof ApiError ? error.message : "Something went wrong. Try again.";
}
