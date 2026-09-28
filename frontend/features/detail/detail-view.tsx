"use client";

import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowLeft,
  Check,
  ChevronDown,
  Edit3,
  ListChecks,
  RefreshCcw,
  Save,
  Trash2,
} from "lucide-react";
import { toast } from "sonner";
import { api } from "@/lib/api/client";
import type {
  BangumiItem,
  EpisodeItem,
  GetRecordData,
  ImdbItem,
  OtherItem,
} from "@/lib/api/types";
import {
  STATUS_LABELS,
  STATUS_OPTIONS,
  TYPE_LABELS,
  statusVariant,
} from "@/lib/constants";
import {
  buildProgress,
  formatSeconds,
  parseProgress,
  progressPercent,
} from "@/lib/progress";
import { navigate } from "@/lib/router";
import { formatDate } from "@/lib/utils";
import { customItemSchema, progressSchema } from "@/lib/validation";
import { CoverImage } from "@/components/common/cover-image";
import { ErrorState } from "@/components/common/async-state";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { Textarea } from "@/components/ui/textarea";

type Source = "bangumi" | "imdb" | "custom";
type MediaInfo = BangumiItem | ImdbItem | OtherItem;

export function DetailView({ source, id }: { source: Source; id: string }) {
  const queryClient = useQueryClient();
  const numericId = Number(id);
  const detailKey = ["detail", source, id] as const;
  const detailQuery = useQuery({
    queryKey: detailKey,
    queryFn: async () => {
      const [infoResponse, recordResponse] =
        source === "custom"
          ? await Promise.all([
              api.getOtherById(numericId),
              api.getRecordByCustom(numericId),
            ])
          : source === "imdb"
            ? await Promise.all([
                api.searchImdbById(id),
                api.getRecordByImdb(id),
              ])
            : await Promise.all([
                api.searchBangumiById(numericId),
                api.getRecordByBangumi(numericId),
              ]);
      if (infoResponse.status !== 0 || !infoResponse.data)
        throw new Error(infoResponse.message || "条目信息加载失败");
      return {
        info: infoResponse.data,
        record:
          recordResponse.status === 0 ? recordResponse.data || null : null,
      };
    },
  });

  if (detailQuery.isLoading) return <DetailSkeleton />;
  if (detailQuery.isError || !detailQuery.data)
    return (
      <>
        <Button variant="ghost" className="mb-5" onClick={() => navigate("/")}>
          <ArrowLeft className="size-4" />
          返回片库
        </Button>
        <ErrorState
          title="无法打开条目"
          message={detailQuery.error?.message}
          onRetry={() => void detailQuery.refetch()}
        />
      </>
    );
  return (
    <DetailContent
      source={source}
      id={id}
      info={detailQuery.data.info}
      record={detailQuery.data.record}
      refresh={() => queryClient.invalidateQueries({ queryKey: detailKey })}
    />
  );
}

function DetailContent({
  source,
  id,
  info,
  record,
  refresh,
}: {
  source: Source;
  id: string;
  info: MediaInfo;
  record: GetRecordData | null;
  refresh: () => Promise<void>;
}) {
  const queryClient = useQueryClient();
  const numericId = Number(id);
  const hasRecord = Boolean(record?.date && !record.is_delete);
  const initialProgress = parseProgress(record?.recorder);
  const [episode, setEpisode] = useState(
    initialProgress.episode === null ? "" : String(initialProgress.episode),
  );
  const [time, setTime] = useState(initialProgress.time || "");
  const [progressError, setProgressError] = useState("");
  const [deleteMode, setDeleteMode] = useState<"soft" | "hard" | null>(null);
  const [episodesOpen, setEpisodesOpen] = useState(false);
  const [editingCustom, setEditingCustom] = useState(false);
  const [custom, setCustom] = useState({
    title: info.title || "",
    description: info.description || "",
    cover: info.cover_url || "",
    maxNumber: info.episodes ? String(info.episodes) : "",
  });
  const updateStatus = useMutation({
    mutationFn: async (status: number) => {
      const response =
        source === "custom"
          ? await api.updateRecordByCustom(numericId, { user_status: status })
          : source === "imdb"
            ? await api.updateRecordByImdb(id, undefined, status)
            : await api.updateRecord(numericId, undefined, status);
      if (response.status !== 0)
        throw new Error(response.message || "状态更新失败");
    },
    onSuccess: async () => {
      toast.success("追踪状态已更新");
      await refresh();
    },
    onError: (error) => toast.error(error.message),
  });
  const updateProgress = useMutation({
    mutationFn: async () => {
      const parsed = progressSchema.safeParse({
        episode: Number(episode),
        time,
      });
      if (!parsed.success)
        throw new Error(parsed.error.issues[0]?.message || "进度格式不正确");
      if (info.episodes && parsed.data.episode > info.episodes)
        throw new Error(`进度不能超过总数 ${info.episodes}`);
      const value = buildProgress(parsed.data.episode, parsed.data.time);
      const response =
        source === "custom"
          ? await api.updateRecordByCustom(numericId, { recorder: value })
          : source === "imdb"
            ? await api.updateRecordByImdb(id, value)
            : await api.updateRecord(numericId, value);
      if (response.status !== 0)
        throw new Error(response.message || "进度更新失败");
    },
    onSuccess: async () => {
      setProgressError("");
      toast.success("进度已记录");
      await refresh();
    },
    onError: (error) => setProgressError(error.message),
  });
  const addRecord = useMutation({
    mutationFn: async () => {
      const status = record?.user_status ?? 1;
      const response =
        source === "custom"
          ? await api.addRecord({ other_id: numericId, user_status: status })
          : source === "imdb"
            ? await api.addRecord({
                source: "imdb",
                external_id: id,
                user_status: status,
              })
            : await api.addRecord({
                bangumi_id: numericId,
                user_status: status,
              });
      if (response.status !== 0)
        throw new Error(response.message || "添加追踪失败");
    },
    onSuccess: async () => {
      toast.success("已加入片库");
      await refresh();
    },
    onError: (error) => toast.error(error.message),
  });
  const deleteRecord = useMutation({
    mutationFn: async (hard: boolean) => {
      const response =
        source === "custom"
          ? await api.deleteRecordByCustom(numericId, hard)
          : source === "imdb"
            ? await api.deleteRecordByImdb(id, hard)
            : await api.deleteRecordByBangumi(numericId, hard);
      if (response.status !== 0)
        throw new Error(response.message || "删除失败");
    },
    onSuccess: () => {
      toast.success("记录已删除");
      navigate("/");
    },
    onError: (error) => toast.error(error.message),
  });
  const saveCustom = useMutation({
    mutationFn: async () => {
      const parsed = customItemSchema.safeParse({
        title: custom.title,
        description: custom.description,
        cover: custom.cover,
        maxNumber: custom.maxNumber ? Number(custom.maxNumber) : undefined,
      });
      if (!parsed.success)
        throw new Error(parsed.error.issues[0]?.message || "请检查条目信息");
      const response = await api.updateRecordByCustom(numericId, {
        other_title: parsed.data.title,
        other_description: parsed.data.description,
        other_cover: parsed.data.cover,
        other_max_number: parsed.data.maxNumber,
      });
      if (response.status !== 0)
        throw new Error(response.message || "保存失败");
    },
    onSuccess: async () => {
      toast.success("条目信息已保存");
      setEditingCustom(false);
      await refresh();
    },
    onError: (error) => toast.error(error.message),
  });

  const episodesQuery = useQuery({
    queryKey: ["episodes", numericId],
    enabled: source === "bangumi" && episodesOpen,
    queryFn: async () => {
      const response = await api.listEpisodes(numericId);
      if (response.status !== 0)
        throw new Error(response.message || "剧集列表加载失败");
      return response.data || [];
    },
  });
  const toggleEpisode = useMutation({
    mutationFn: async (item: EpisodeItem) => {
      const response = await api.updateEpisode(numericId, item.ordinal, {
        watched: !item.watched,
      });
      if (response.status !== 0 || !response.data)
        throw new Error(response.message || "剧集更新失败");
      return response.data;
    },
    onSuccess: async (updated) => {
      queryClient.setQueryData<EpisodeItem[]>(
        ["episodes", numericId],
        (current) =>
          current?.map((item) =>
            item.ordinal === updated.ordinal ? updated : item,
          ),
      );
      await refresh();
    },
    onError: (error) => toast.error(error.message),
  });
  const forceEpisodes = useMutation({
    mutationFn: async () => {
      const response = await api.listEpisodes(numericId, true);
      if (response.status !== 0)
        throw new Error(response.message || "刷新失败");
      return response.data || [];
    },
    onSuccess: (data) => {
      queryClient.setQueryData(["episodes", numericId], data);
      toast.success("剧集数据已刷新");
    },
    onError: (error) => toast.error(error.message),
  });

  const forceBangumiContent = useMutation({
    mutationFn: async () => {
      const response = await api.refreshBangumi(numericId);
      if (response.status !== 0 || !response.data)
        throw new Error(response.message || "内容刷新失败");
      return response.data;
    },
    onSuccess: async () => {
      await refresh();
      toast.success("条目内容已刷新");
    },
    onError: (error) => toast.error(error.message),
  });

  const percent = progressPercent(record?.recorder, info.episodes);
  return (
    <>
      <Button
        variant="ghost"
        className="mb-5 -ml-3"
        onClick={() => navigate("/")}
      >
        <ArrowLeft className="size-4" />
        返回片库
      </Button>
      <article className="grid gap-7 xl:grid-cols-[minmax(0,1fr)_380px]">
        <div className="min-w-0">
          <section className="grid gap-6 sm:grid-cols-[180px_minmax(0,1fr)]">
            <CoverImage
              src={info.cover_url}
              alt={info.title || "条目封面"}
              priority
              className="w-full max-w-[220px] rounded-2xl shadow-popover"
              sizes="220px"
            />
            <div className="min-w-0 pt-1">
              <div className="mb-3 flex flex-wrap gap-2">
                <Badge variant="outline">
                  {source === "imdb"
                    ? "IMDb"
                    : source === "custom"
                      ? "自定义"
                      : "Bangumi"}
                </Badge>
                <Badge>{TYPE_LABELS[info.type] || "其他"}</Badge>
                {hasRecord ? (
                  <Badge variant={statusVariant(record?.user_status)}>
                    {STATUS_LABELS[record?.user_status ?? -1] || "未设置"}
                  </Badge>
                ) : null}
              </div>
              {source === "bangumi" ? (
                <Button
                  variant="outline"
                  size="sm"
                  loading={forceBangumiContent.isPending}
                  onClick={() => forceBangumiContent.mutate()}
                >
                  <RefreshCcw className="size-4" />
                  刷新内容
                </Button>
              ) : null}
              <h1 className="text-pretty font-display text-4xl font-semibold leading-[1.02] tracking-[-0.035em] sm:text-5xl">
                {info.title || "未命名条目"}
              </h1>
              <dl className="mt-6 grid gap-3 text-sm sm:grid-cols-2">
                {info.author ? (
                  <Meta
                    label={source === "imdb" ? "创作者" : "原作"}
                    value={info.author}
                  />
                ) : null}
                {info.release_date ? (
                  <Meta label="发行日期" value={info.release_date} />
                ) : null}
                {info.episodes ? (
                  <Meta
                    label="总数"
                    value={`${info.episodes} ${source === "custom" ? "项" : "话"}`}
                  />
                ) : null}
                <Meta
                  label="资料来源"
                  value={
                    source === "imdb"
                      ? "IMDb"
                      : source === "custom"
                        ? "本地自定义"
                        : "Bangumi"
                  }
                />
              </dl>
            </div>
          </section>
          {info.description ? (
            <section className="mt-8 border-t border-border pt-7">
              <h2 className="font-display text-2xl font-semibold">简介</h2>
              <p className="mt-3 whitespace-pre-wrap text-pretty leading-8 text-muted-foreground">
                {info.description}
              </p>
            </section>
          ) : null}
          {source === "bangumi" ? (
            <section className="mt-8 border-t border-border pt-7">
              <div className="flex flex-wrap items-center justify-between gap-3">
                <button
                  className="flex min-h-11 items-center gap-2 rounded-lg font-display text-2xl font-semibold focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
                  aria-expanded={episodesOpen}
                  onClick={() => setEpisodesOpen((value) => !value)}
                >
                  <ListChecks className="size-5 text-primary" />
                  单集记录
                  <ChevronDown
                    className={
                      episodesOpen
                        ? "size-4 rotate-180 transition"
                        : "size-4 transition"
                    }
                  />
                </button>
                {episodesOpen ? (
                  <Button
                    variant="outline"
                    size="sm"
                    loading={forceEpisodes.isPending}
                    onClick={() => forceEpisodes.mutate()}
                  >
                    <RefreshCcw className="size-4" />
                    刷新元数据
                  </Button>
                ) : null}
              </div>
              {episodesOpen ? (
                <EpisodeList
                  items={episodesQuery.data || []}
                  loading={episodesQuery.isLoading}
                  error={episodesQuery.error?.message}
                  updatingOrdinal={
                    toggleEpisode.isPending
                      ? toggleEpisode.variables?.ordinal
                      : undefined
                  }
                  onToggle={(item) => toggleEpisode.mutate(item)}
                />
              ) : null}
            </section>
          ) : null}
        </div>
        <aside className="xl:sticky xl:top-10 xl:self-start">
          <Card>
            <CardHeader>
              <div className="flex items-center justify-between">
                <div>
                  <CardTitle>
                    {hasRecord ? "追踪控制台" : "加入我的片库"}
                  </CardTitle>
                  <CardDescription>
                    {hasRecord
                      ? `上次更新 ${formatDate(record?.date, true)}`
                      : "选择初始状态后开始记录"}
                  </CardDescription>
                </div>
                {source === "custom" && hasRecord ? (
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label="编辑自定义条目"
                    onClick={() => setEditingCustom(true)}
                  >
                    <Edit3 className="size-4" />
                  </Button>
                ) : null}
              </div>
            </CardHeader>
            <CardContent>
              <div>
                <div className="mb-3 flex items-center justify-between text-sm">
                  <span className="font-semibold">当前进度</span>
                  <span className="font-mono text-xs text-muted-foreground">
                    {record?.recorder
                      ? `EP ${record.recorder.replace("|", " · ")}`
                      : "未记录"}
                  </span>
                </div>
                <div className="flex gap-1">
                  {Array.from({ length: 16 }, (_, index) => (
                    <span
                      key={index}
                      className={
                        index < Math.round(percent / 6.25)
                          ? "h-2 flex-1 rounded-full bg-accent"
                          : "h-2 flex-1 rounded-full bg-muted"
                      }
                    />
                  ))}
                </div>
                {info.episodes ? (
                  <div className="mt-2 text-right font-mono text-[10px] text-muted-foreground">
                    {percent}% / {info.episodes}
                  </div>
                ) : null}
              </div>
              <Separator className="my-6" />
              <div>
                <Label>追踪状态</Label>
                <div className="mt-3 grid grid-cols-3 gap-2">
                  {STATUS_OPTIONS.map((option) => (
                    <button
                      key={option.value}
                      disabled={!hasRecord || updateStatus.isPending}
                      aria-pressed={record?.user_status === option.value}
                      className={
                        record?.user_status === option.value
                          ? "min-h-10 rounded-lg bg-foreground px-2 text-xs font-semibold text-background"
                          : "min-h-10 rounded-lg border border-border px-2 text-xs font-semibold text-muted-foreground transition hover:text-foreground disabled:cursor-not-allowed disabled:opacity-50"
                      }
                      onClick={() => updateStatus.mutate(option.value)}
                    >
                      {option.label}
                    </button>
                  ))}
                </div>
              </div>
              {hasRecord ? (
                <>
                  <Separator className="my-6" />
                  <form
                    onSubmit={(event) => {
                      event.preventDefault();
                      updateProgress.mutate();
                    }}
                  >
                    <Label>记录进度</Label>
                    <div className="mt-3 grid grid-cols-[1fr_1fr] gap-3">
                      <div>
                        <Label htmlFor="detail-episode" className="sr-only">
                          集数
                        </Label>
                        <Input
                          id="detail-episode"
                          type="number"
                          min="0"
                          max={info.episodes || undefined}
                          inputMode="numeric"
                          placeholder="集数"
                          value={episode}
                          onChange={(event) => setEpisode(event.target.value)}
                        />
                      </div>
                      <div>
                        <Label htmlFor="detail-time" className="sr-only">
                          时间
                        </Label>
                        <Input
                          id="detail-time"
                          placeholder="mm:ss"
                          value={time}
                          onChange={(event) => setTime(event.target.value)}
                        />
                      </div>
                    </div>
                    <p
                      aria-live="polite"
                      className="mt-2 min-h-5 text-xs font-medium text-destructive"
                    >
                      {progressError}
                    </p>
                    <Button
                      type="submit"
                      className="mt-1 w-full"
                      loading={updateProgress.isPending}
                    >
                      <Save className="size-4" />
                      保存进度
                    </Button>
                  </form>
                  <div className="mt-3 grid grid-cols-2 gap-2">
                    <Button
                      variant="outline"
                      onClick={() => setDeleteMode("soft")}
                    >
                      <Trash2 className="size-4" />
                      软删除
                    </Button>
                    <Button
                      variant="ghost"
                      className="text-destructive hover:text-destructive"
                      onClick={() => setDeleteMode("hard")}
                    >
                      永久删除
                    </Button>
                  </div>
                </>
              ) : (
                <Button
                  className="mt-6 w-full"
                  size="lg"
                  loading={addRecord.isPending}
                  onClick={() => addRecord.mutate()}
                >
                  添加追踪
                </Button>
              )}
            </CardContent>
          </Card>
        </aside>
      </article>
      <AlertDialog
        open={Boolean(deleteMode)}
        onOpenChange={(open) => !open && setDeleteMode(null)}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>
              {deleteMode === "hard" ? "永久删除记录？" : "从片库移除？"}
            </AlertDialogTitle>
            <AlertDialogDescription>
              {deleteMode === "hard"
                ? "所有追踪数据将被永久删除，无法恢复。日志仍会保留用于审计。"
                : "记录会标记为软删除，稍后重新添加可以恢复。"}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>取消</AlertDialogCancel>
            <AlertDialogAction
              onClick={(event) => {
                event.preventDefault();
                deleteRecord.mutate(deleteMode === "hard");
              }}
            >
              {deleteRecord.isPending ? "删除中…" : "确认删除"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
      <Dialog open={editingCustom} onOpenChange={setEditingCustom}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>编辑自定义条目</DialogTitle>
            <DialogDescription>
              修改条目资料不会清除已记录的追踪进度。
            </DialogDescription>
          </DialogHeader>
          <div className="grid gap-4">
            <Field label="名称" htmlFor="edit-title">
              <Input
                id="edit-title"
                value={custom.title}
                onChange={(event) =>
                  setCustom({ ...custom, title: event.target.value })
                }
              />
            </Field>
            <Field label="描述" htmlFor="edit-description">
              <Textarea
                id="edit-description"
                value={custom.description}
                onChange={(event) =>
                  setCustom({ ...custom, description: event.target.value })
                }
              />
            </Field>
            <Field label="封面 URL" htmlFor="edit-cover">
              <Input
                id="edit-cover"
                value={custom.cover}
                onChange={(event) =>
                  setCustom({ ...custom, cover: event.target.value })
                }
              />
            </Field>
            <Field label="总数" htmlFor="edit-total">
              <Input
                id="edit-total"
                type="number"
                min="0"
                value={custom.maxNumber}
                onChange={(event) =>
                  setCustom({ ...custom, maxNumber: event.target.value })
                }
              />
            </Field>
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setEditingCustom(false)}>
              取消
            </Button>
            <Button
              loading={saveCustom.isPending}
              onClick={() => saveCustom.mutate()}
            >
              保存更改
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}

function Meta({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className="font-mono text-[10px] uppercase tracking-[0.15em] text-muted-foreground">
        {label}
      </dt>
      <dd className="mt-1.5 font-medium">{value}</dd>
    </div>
  );
}
function Field({
  label,
  htmlFor,
  children,
}: {
  label: string;
  htmlFor: string;
  children: React.ReactNode;
}) {
  return (
    <div className="grid gap-2">
      <Label htmlFor={htmlFor}>{label}</Label>
      {children}
    </div>
  );
}

function EpisodeList({
  items,
  loading,
  error,
  updatingOrdinal,
  onToggle,
}: {
  items: EpisodeItem[];
  loading: boolean;
  error?: string;
  updatingOrdinal?: number;
  onToggle: (item: EpisodeItem) => void;
}) {
  if (loading)
    return (
      <div className="mt-5 grid gap-2">
        {Array.from({ length: 6 }, (_, index) => (
          <Skeleton key={index} className="h-14" />
        ))}
      </div>
    );
  if (error)
    return (
      <p
        role="alert"
        className="mt-5 rounded-xl border border-destructive/20 bg-destructive/10 p-4 text-sm text-destructive"
      >
        {error}
      </p>
    );
  if (!items.length)
    return (
      <p className="mt-5 rounded-xl border border-dashed border-border p-8 text-center text-sm text-muted-foreground">
        暂无剧集数据
      </p>
    );
  return (
    <div className="mt-5 grid gap-2">
      {items.map((item) => (
        <button
          key={item.ordinal}
          disabled={updatingOrdinal === item.ordinal}
          className={
            item.watched
              ? "grid min-h-14 grid-cols-[24px_54px_minmax(0,1fr)_auto] items-center gap-3 rounded-xl border border-accent/25 bg-accent/10 px-3 text-left transition focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
              : "grid min-h-14 grid-cols-[24px_54px_minmax(0,1fr)_auto] items-center gap-3 rounded-xl border border-border bg-card px-3 text-left transition hover:border-foreground/20 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
          }
          onClick={() => onToggle(item)}
        >
          <Checkbox checked={item.watched} tabIndex={-1} aria-hidden />
          <span className="font-mono text-xs font-semibold">
            EP {item.label || item.ordinal}
          </span>
          <span className="truncate text-sm">
            {item.name_cn || item.title || `第 ${item.ordinal} 集`}
          </span>
          <span className="hidden text-xs text-muted-foreground sm:block">
            {item.progress_seconds
              ? formatSeconds(item.progress_seconds)
              : item.airdate ||
                (item.watched ? (
                  <Check className="size-4 text-success" />
                ) : (
                  "未看"
                ))}
          </span>
        </button>
      ))}
    </div>
  );
}

function DetailSkeleton() {
  return (
    <div>
      <Skeleton className="mb-6 h-11 w-32" />
      <div className="grid gap-7 xl:grid-cols-[1fr_380px]">
        <div className="flex gap-6">
          <Skeleton className="h-[280px] w-[210px] shrink-0" />
          <div className="flex-1">
            <Skeleton className="h-12 w-4/5" />
            <Skeleton className="mt-5 h-5 w-1/2" />
            <Skeleton className="mt-8 h-24 w-full" />
          </div>
        </div>
        <Skeleton className="h-[480px]" />
      </div>
    </div>
  );
}
