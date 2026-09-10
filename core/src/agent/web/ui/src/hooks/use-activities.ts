import { useCallback, useEffect, useRef, useState } from "react";

import {
  activityApi,
  hasLiveActivityJobs,
  type ActivityDetail,
  type ActivityState,
} from "@/lib/activities";

export function activityError(cause: unknown): string {
  return cause instanceof Error ? cause.message : "The Activity request failed.";
}

function useActivityView<T>(
  read: (signal: AbortSignal) => Promise<T>,
  interval: (value: T | null) => number,
) {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const sequence = useRef(0);
  const controller = useRef<AbortController | null>(null);
  const timer = useRef<number>();

  const refresh = useCallback(async (): Promise<T | null> => {
    window.clearTimeout(timer.current);
    controller.current?.abort();
    const abort = new AbortController();
    controller.current = abort;
    const request = ++sequence.current;
    const current = () => request === sequence.current && !abort.signal.aborted;
    let value: T | null = null;
    setLoading(true);
    try {
      value = await read(abort.signal);
      if (!current()) return null;
      setData(value);
      setError(null);
      return value;
    } catch (cause) {
      if (current()) setError(activityError(cause));
      return null;
    } finally {
      if (current()) {
        setLoading(false);
        timer.current = window.setTimeout(() => void refresh(), interval(value));
      }
    }
  }, [read, interval]);

  useEffect(() => {
    setData(null);
    setError(null);
    void refresh();
    const onChange = () => void refresh();
    window.addEventListener("focus", onChange);
    window.addEventListener("cos:notifications-changed", onChange);
    return () => {
      ++sequence.current;
      controller.current?.abort();
      window.clearTimeout(timer.current);
      window.removeEventListener("focus", onChange);
      window.removeEventListener("cos:notifications-changed", onChange);
    };
  }, [refresh]);

  return { data, loading, error, refresh };
}

const listInterval = () => 10_000;
const detailInterval = (detail: ActivityDetail | null) =>
  detail && hasLiveActivityJobs(detail.jobs) ? 3_000 : 10_000;

export function useActivities(state?: ActivityState) {
  const read = useCallback((signal: AbortSignal) => activityApi.list(state, signal), [state]);
  return useActivityView(read, listInterval);
}

export function useActivity(id: string) {
  const read = useCallback((signal: AbortSignal) => activityApi.get(id, signal), [id]);
  return useActivityView(read, detailInterval);
}
