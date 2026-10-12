// Typed wrappers around the app's commands and events. The page has no
// network, file or shell access of its own; everything goes through these.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Settings {
  window_days: number;
  locations: string[] | null;
  labels_on: boolean;
  check_time: [number, number];
  background_on: boolean;
  setup_done: boolean;
}

export interface LocationView {
  id: string;
  name: string;
  active: boolean;
}

export interface LastCheck {
  started_at: string | null;
  ended_at: string | null;
  trigger: string;
  outcome: string;
  detail: string | null;
}

export interface Status {
  business_name: string | null;
  synced: boolean;
  disconnected: boolean;
  last_check: LastCheck | null;
  last_success: string | null;
  labels_in_square: number;
  halted: { name: string; at: string; paths: string[] } | null;
  failure_count: number;
}

export interface AppView {
  configured: boolean;
  connected: boolean;
  has_write_scope: boolean;
  business_name: string | null;
  settings: Settings;
  locations: LocationView[];
  status: Status;
  next_check: string | null;
  registration: { method: string; detail: string };
  version: string;
  environment: string;
}

export interface ReportRow {
  item_id: string;
  variation_id: string;
  item: string;
  variation: string;
  sku: string | null;
  category: string | null;
  last_sold: string;
  last_sold_date: string | null;
  days_since: number | null;
  slow: boolean;
  on_hand: number | null;
  currency: string | null;
  value_at_price: number | null;
  value_at_cost: number | null;
  label_status: string;
}

export interface Report {
  rows: ReportRow[];
  window_days: number;
  categories: string[];
  slow_count: number;
  items_shown: number;
}

export interface Query {
  window_days: number | null;
  location: string | null;
  category: string | null;
  slow_only: boolean;
}

export interface Summary {
  business_name: string;
  first_sync: boolean;
  orders_read: number;
  labels_written: number;
  labels_unchanged: number;
  labels_skipped: number;
  label_problem: string | null;
}

export type Outcome =
  | { outcome: "done"; detail: Summary }
  | { outcome: "skipped"; detail: string }
  | { outcome: "not_connected" }
  | { outcome: "disconnected" }
  | { outcome: "failed"; detail: string };

export type ProgressEvent =
  | { kind: "catalog"; objects: number }
  | { kind: "sales"; months_done: number; months_total: number }
  | { kind: "inventory" }
  | { kind: "labels" };

export interface Halt {
  object_id: string;
  name: string;
  at: string;
  paths: string[];
  fetched: unknown;
  sent: unknown;
  stored: unknown;
}

export interface SelfTestReport {
  passed: boolean;
  steps: { name: string; ok: boolean; detail: string }[];
  millis: number;
  requests: number;
  version: string;
  square_version: string;
}

export const api = {
  view: () => invoke<AppView>("app_view"),
  connect: (labelsOn: boolean) => invoke<void>("connect", { labelsOn }),
  cancelConnect: () => invoke<void>("cancel_connect"),
  saveSettings: (settings: Settings, start: boolean) =>
    invoke<{ view: AppView; needs_reconnect: boolean; registration_error: string | null }>("save_settings", {
      settings,
      start,
    }),
  checkNow: () => invoke<void>("check_now"),
  report: (query: Query) => invoke<Report>("report", { query }),
  exportReport: (query: Query, format: "xlsx" | "csv") => invoke<string | null>("export", { query, format }),
  removeLabels: () => invoke<void>("remove_labels"),
  haltDetails: () => invoke<Halt | null>("halt_details"),
  resumeLabels: () => invoke<AppView>("resume_labels"),
  disconnect: () => invoke<AppView>("disconnect"),
  selfTest: () => invoke<void>("self_test"),
  diagnostics: () => invoke<string>("diagnostics"),
  saveDiagnostics: (text: string) => invoke<string | null>("save_diagnostics", { text }),
  pageError: (message: string) => invoke<void>("page_error", { message }),
};

export function on<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (e) => handler(e.payload));
}
