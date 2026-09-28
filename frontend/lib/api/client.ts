import { clearStoredSession, getStoredToken } from "@/lib/auth-storage";
import type {
  AddRecordData,
  AddRecordParams,
  ApiResponse,
  ApiTokenItem,
  AutoCleanupSetting,
  BangumiItem,
  BangumiSearchItem,
  ConfigData,
  CreateTokenData,
  DetailListItem,
  EpisodeItem,
  GetRecordData,
  ImdbItem,
  ImdbSearchItem,
  LocalSearchResult,
  LogListData,
  LoginData,
  OtherItem,
  PermissionLabelsResponse,
  RecordingLogFilters,
  RecordingLogItem,
  RegisterData,
  SystemLogFilters,
  SystemLogItem,
  UserInfo,
} from "@/lib/api/types";

const REQUEST_TIMEOUT_MS = 15_000;
const API_BASE = process.env.NEXT_PUBLIC_API_BASE_URL?.replace(/\/$/, "") ?? "";
let sessionController = new AbortController();

export function cancelSessionRequests() {
  sessionController.abort();
  sessionController = new AbortController();
}

export class ApiNetworkError extends Error {
  constructor(
    message: string,
    public readonly kind:
      | "offline"
      | "timeout"
      | "network"
      | "invalid-response",
  ) {
    super(message);
    this.name = "ApiNetworkError";
  }
}

interface RequestOptions {
  method?: "GET" | "POST" | "PUT" | "PATCH" | "DELETE";
  body?: unknown;
  signal?: AbortSignal;
}

async function request<T>(
  url: string,
  options: RequestOptions = {},
): Promise<T> {
  if (typeof navigator !== "undefined" && !navigator.onLine)
    throw new ApiNetworkError("当前没有网络连接", "offline");
  const controller = new AbortController();
  const sessionSignal = sessionController.signal;
  const token = getStoredToken();
  const timeout = window.setTimeout(
    () => controller.abort("timeout"),
    REQUEST_TIMEOUT_MS,
  );
  const abortFromCaller = () => controller.abort(options.signal?.reason);
  const abortFromSession = () => controller.abort();
  options.signal?.addEventListener("abort", abortFromCaller, { once: true });
  sessionSignal.addEventListener("abort", abortFromSession, { once: true });
  if (options.signal?.aborted) abortFromCaller();

  try {
    controller.signal.throwIfAborted();
    const headers: HeadersInit = { Accept: "application/json" };
    if (options.body !== undefined)
      headers["Content-Type"] = "application/json";
    if (token) headers.Authorization = `Bearer ${token}`;
    const response = await fetch(`${API_BASE}${url}`, {
      method: options.method ?? "GET",
      headers,
      body:
        options.body === undefined ? undefined : JSON.stringify(options.body),
      signal: controller.signal,
    });

    const text = await response.text();
    // Ignore late responses even if the transport did not honor cancellation.
    controller.signal.throwIfAborted();
    let data: unknown = {
      status: response.ok ? 0 : -1,
      message: response.statusText,
    };
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        throw new ApiNetworkError(
          "服务器返回了无法识别的数据",
          "invalid-response",
        );
      }
    }

    if (response.status === 401 && token === getStoredToken()) {
      clearStoredSession();
      window.dispatchEvent(new Event("bangumi-recorder:session-expired"));
    }
    return data as T;
  } catch (error) {
    if (error instanceof ApiNetworkError) throw error;
    if (controller.signal.aborted)
      throw new ApiNetworkError(
        controller.signal.reason === "timeout"
          ? "请求超时，请稍后重试"
          : "请求已取消",
        "timeout",
      );
    throw new ApiNetworkError("无法连接到后端服务", "network");
  } finally {
    window.clearTimeout(timeout);
    options.signal?.removeEventListener("abort", abortFromCaller);
    sessionSignal.removeEventListener("abort", abortFromSession);
  }
}

function queryString(
  values: Record<string, string | number | boolean | undefined>,
) {
  const params = new URLSearchParams();
  Object.entries(values).forEach(([key, value]) => {
    if (value !== undefined && value !== "") params.set(key, String(value));
  });
  const query = params.toString();
  return query ? `?${query}` : "";
}

export const api = {
  login: (username: string, password: string) =>
    request<ApiResponse<LoginData>>("/api/v2/auth/login", {
      method: "POST",
      body: { username, password },
    }),
  register: (username: string, password: string, registerToken?: string) =>
    request<ApiResponse<RegisterData>>("/api/v2/auth/register", {
      method: "POST",
      body: { username, password, register_token: registerToken },
    }),
  getConfig: () => request<ApiResponse<ConfigData>>("/api/v2/auth/config"),

  getUserInfo: (signal?: AbortSignal) =>
    request<ApiResponse<UserInfo>>("/api/v2/me", { signal }),
  updateUserInfo: (nickname?: string, avatar?: string) =>
    request<ApiResponse<null>>("/api/v2/me", {
      method: "PATCH",
      body: { nickname, avatar },
    }),
  updatePassword: (oldPassword: string, newPassword: string) =>
    request<ApiResponse<null>>("/api/v2/me/password", {
      method: "PUT",
      body: { old_password: oldPassword, new_password: newPassword },
    }),

  getDetailList: (signal?: AbortSignal) =>
    request<ApiResponse<DetailListItem[]>>("/api/v2/records/detail", {
      signal,
    }),
  addRecord: (params: AddRecordParams) =>
    request<ApiResponse<AddRecordData>>("/api/v2/records", {
      method: "POST",
      body: params,
    }),
  getRecordByBangumi: (id: number) =>
    request<ApiResponse<GetRecordData>>(`/api/v2/records/bangumi/${id}`),
  getRecordByImdb: (id: string) =>
    request<ApiResponse<GetRecordData>>(
      `/api/v2/records/imdb/${encodeURIComponent(id)}`,
    ),
  getRecordByCustom: (id: number) =>
    request<ApiResponse<GetRecordData>>(`/api/v2/records/custom/${id}`),
  updateRecord: (id: number, recorder?: string, userStatus?: number) =>
    request<ApiResponse<null>>(`/api/v2/records/bangumi/${id}`, {
      method: "PATCH",
      body: { recorder, user_status: userStatus },
    }),
  updateRecordByImdb: (id: string, recorder?: string, userStatus?: number) =>
    request<ApiResponse<null>>(
      `/api/v2/records/imdb/${encodeURIComponent(id)}`,
      { method: "PATCH", body: { recorder, user_status: userStatus } },
    ),
  updateRecordByCustom: (
    id: number,
    data: {
      recorder?: string;
      user_status?: number;
      other_title?: string;
      other_description?: string;
      other_cover?: string;
      other_max_number?: number;
      other_status?: number;
    },
  ) =>
    request<ApiResponse<null>>(`/api/v2/records/custom/${id}`, {
      method: "PATCH",
      body: data,
    }),
  deleteRecordByBangumi: (id: number, hardDelete = false) =>
    request<ApiResponse<null>>(
      `/api/v2/records/bangumi/${id}${queryString({ hard_delete: hardDelete || undefined })}`,
      { method: "DELETE" },
    ),
  deleteRecordByImdb: (id: string, hardDelete = false) =>
    request<ApiResponse<null>>(
      `/api/v2/records/imdb/${encodeURIComponent(id)}${queryString({ hard_delete: hardDelete || undefined })}`,
      { method: "DELETE" },
    ),
  deleteRecordByCustom: (id: number, hardDelete = false) =>
    request<ApiResponse<null>>(
      `/api/v2/records/custom/${id}${queryString({ hard_delete: hardDelete || undefined })}`,
      { method: "DELETE" },
    ),

  searchBangumi: (title: string, page = 1) =>
    request<ApiResponse<BangumiSearchItem[]>>(
      `/api/v2/search${queryString({ q: title, page })}`,
    ),
  searchBangumiById: (id: number, force = false) =>
    request<ApiResponse<BangumiItem>>(
      `/api/v2/bangumi/${id}${queryString({ force: force || undefined })}`,
    ),
  refreshBangumi: (id: number) =>
    request<ApiResponse<BangumiItem>>(`/api/v2/bangumi/${id}/refresh`, {
      method: "POST",
    }),

  searchImdb: (title: string, page = 1, useApi = false) =>
    request<ApiResponse<ImdbSearchItem[]>>(
      `/api/v2/imdb/search${queryString({ q: title, page, use_api: useApi })}`,
    ),
  searchImdbById: (id: string, force = false, useApi = false) =>
    request<ApiResponse<ImdbItem>>(
      `/api/v2/imdb/${encodeURIComponent(id)}${queryString({ force: force || undefined, use_api: useApi })}`,
    ),
  getOtherById: (id: number) =>
    request<ApiResponse<OtherItem>>(`/api/v2/other/${id}`),
  searchLocal: (keyword?: string, id?: number, page = 1, pageSize = 20) =>
    request<ApiResponse<LocalSearchResult>>(
      `/api/v2/search/local${queryString({ q: keyword, id, page, page_size: pageSize })}`,
    ),

  listEpisodes: (bangumiId: number, force = false) =>
    request<ApiResponse<EpisodeItem[]>>(
      `/api/v2/records/bangumi/${bangumiId}/episodes${queryString({ force: force || undefined })}`,
    ),
  updateEpisode: (
    bangumiId: number,
    ordinal: number,
    data: {
      watched?: boolean;
      progress_seconds?: number;
      duration_seconds?: number;
    },
  ) =>
    request<ApiResponse<EpisodeItem>>(
      `/api/v2/records/bangumi/${bangumiId}/episodes/${ordinal}`,
      { method: "PATCH", body: data },
    ),

  listRecordingLogs: (
    page = 1,
    pageSize = 50,
    filters: RecordingLogFilters = {},
  ) =>
    request<ApiResponse<LogListData<RecordingLogItem>>>(
      `/api/v2/logs/recordings${queryString({ page, page_size: pageSize, ...filters })}`,
    ),
  listSystemLogs: (page = 1, pageSize = 50, filters: SystemLogFilters = {}) =>
    request<ApiResponse<LogListData<SystemLogItem>>>(
      `/api/v2/logs/system${queryString({ page, page_size: pageSize, ...filters })}`,
    ),
  getAutoCleanupSetting: () =>
    request<ApiResponse<AutoCleanupSetting>>("/api/v2/settings/auto-cleanup"),
  updateAutoCleanupSetting: (enabled: boolean) =>
    request<ApiResponse<null>>("/api/v2/settings/auto-cleanup", {
      method: "PUT",
      body: { enabled },
    }),

  listTokens: () => request<ApiResponse<ApiTokenItem[]>>("/api/v2/tokens"),
  createToken: (name: string, permissions: number) =>
    request<ApiResponse<CreateTokenData>>("/api/v2/tokens", {
      method: "POST",
      body: { name, permissions },
    }),
  updateToken: (
    id: number,
    data: { name?: string; permissions?: number; is_active?: boolean },
  ) =>
    request<ApiResponse<null>>(`/api/v2/tokens/${id}`, {
      method: "PUT",
      body: data,
    }),
  deleteToken: (id: number) =>
    request<ApiResponse<null>>(`/api/v2/tokens/${id}`, { method: "DELETE" }),
  getPermissionLabels: () =>
    request<ApiResponse<PermissionLabelsResponse>>(
      "/api/v2/tokens/permissions",
    ),
};
