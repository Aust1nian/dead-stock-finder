// The report: one table, filters, a summary, and Save. Rows are drawn only
// while on screen, so 20,000 items scroll as smoothly as 20.

import { api, type AppView, type Query, type Report, type ReportRow } from "./api";
import { clear, h } from "./dom";
import { money, plural, quantity } from "./format";

const ROW_HEIGHT = 40;

type SortKey = "item" | "variation" | "category" | "last_sold" | "days" | "on_hand" | "value" | "label";

interface Column {
  key: SortKey;
  title: string;
  cls?: string;
}

const COLUMNS: Column[] = [
  { key: "item", title: "Item" },
  { key: "variation", title: "Variation" },
  { key: "category", title: "Category" },
  { key: "last_sold", title: "Last sold" },
  { key: "days", title: "Days since", cls: "num" },
  { key: "on_hand", title: "On hand", cls: "num" },
  { key: "value", title: "Value tied up", cls: "num" },
  { key: "label", title: "Label" },
];

/** No sale at all sorts as the oldest. */
function daysOf(r: ReportRow): number {
  if (r.days_since !== null) return r.last_sold_date ? r.days_since : r.days_since + 0.5;
  return Number.MAX_SAFE_INTEGER;
}

function compare(a: ReportRow, b: ReportRow, key: SortKey): number {
  switch (key) {
    case "item":
      return a.item.localeCompare(b.item) || a.variation.localeCompare(b.variation);
    case "variation":
      return a.variation.localeCompare(b.variation);
    case "category":
      return (a.category ?? "").localeCompare(b.category ?? "");
    case "last_sold":
    case "days":
      return daysOf(a) - daysOf(b);
    case "on_hand":
      return (a.on_hand ?? -1) - (b.on_hand ?? -1);
    case "value":
      return (a.value_at_price ?? -1) - (b.value_at_price ?? -1);
    case "label":
      return a.label_status.localeCompare(b.label_status);
  }
}

export class ReportView {
  readonly el: HTMLElement;
  private view: AppView;
  private report: Report | null = null;
  private rows: ReportRow[] = [];
  private sort: { key: SortKey; desc: boolean } = { key: "days", desc: true };
  private query: Query;
  private search = "";
  private body!: HTMLElement;
  private spacer!: HTMLElement;
  private summary!: HTMLElement;
  private header!: HTMLElement;
  private notice!: HTMLElement;
  private scroller!: HTMLElement;
  private categorySelect!: HTMLSelectElement;

  constructor(view: AppView) {
    this.view = view;
    this.query = { window_days: view.settings.window_days, location: null, category: null, slow_only: true };
    this.el = h("section", { class: "report", "aria-label": "Report" });
    this.build();
  }

  setView(view: AppView): void {
    this.view = view;
  }

  private build(): void {
    const windowInput = h("input", {
      type: "number",
      min: "1",
      max: "3650",
      value: String(this.query.window_days),
      "aria-label": "Window in days",
      id: "filter-window",
      onchange: (e: Event) => {
        const v = parseInt((e.target as HTMLInputElement).value, 10);
        this.query.window_days = Number.isFinite(v) && v > 0 ? v : this.view.settings.window_days;
        void this.load();
      },
    });
    const selected = this.view.locations.filter((l) =>
      this.view.settings.locations ? this.view.settings.locations.includes(l.id) : l.active,
    );
    const location = h(
      "select",
      {
        id: "filter-location",
        "aria-label": "Location",
        onchange: (e: Event) => {
          this.query.location = (e.target as HTMLSelectElement).value || null;
          void this.load();
        },
      },
      h("option", { value: "" }, selected.length > 1 ? "All selected locations" : selected[0]?.name ?? "All locations"),
      ...(selected.length > 1 ? selected.map((l) => h("option", { value: l.id }, l.name)) : []),
    );
    const category = h("select", {
      id: "filter-category",
      "aria-label": "Category",
      onchange: (e: Event) => {
        this.query.category = (e.target as HTMLSelectElement).value || null;
        void this.load();
      },
    });
    const slow = h("input", {
      type: "checkbox",
      id: "filter-slow",
      checked: this.query.slow_only,
      onchange: (e: Event) => {
        this.query.slow_only = (e.target as HTMLInputElement).checked;
        void this.load();
      },
    });
    const search = h("input", {
      type: "search",
      placeholder: "Find an item or SKU",
      "aria-label": "Find an item or SKU",
      id: "filter-search",
      oninput: (e: Event) => {
        this.search = (e.target as HTMLInputElement).value.trim().toLowerCase();
        this.arrange();
      },
    });
    const saveMenu = h(
      "div",
      { class: "menu", id: "save-menu", hidden: true },
      h("button", { onclick: () => this.save("xlsx"), id: "save-xlsx" }, "Excel workbook (.xlsx)"),
      h("button", { onclick: () => this.save("csv"), id: "save-csv" }, "CSV (.csv)"),
    );
    const save = h(
      "div",
      { class: "menu-anchor" },
      h(
        "button",
        {
          class: "secondary",
          id: "save",
          "aria-haspopup": "true",
          onclick: () => {
            saveMenu.hidden = !saveMenu.hidden;
          },
        },
        "Save…",
      ),
      saveMenu,
    );
    document.addEventListener("click", (e) => {
      if (!(e.target as Element).closest(".menu-anchor")) saveMenu.hidden = true;
    });
    const filters = h(
      "div",
      { class: "filters" },
      h("label", { class: "field" }, "No sale in ", windowInput, " days"),
      h("label", { class: "field" }, location),
      h("label", { class: "field" }, category),
      h("label", { class: "check" }, slow, " Slow only"),
      h("span", { class: "grow" }),
      search,
      save,
    );
    this.summary = h("p", { class: "summary", id: "summary", "aria-live": "polite" });
    this.notice = h("p", { class: "notice", hidden: true });
    this.header = h("div", { class: "thead", role: "row" });
    this.renderHeader();
    this.spacer = h("div", { class: "spacer" });
    this.body = h("div", { class: "tbody", role: "rowgroup" });
    const scroller = h("div", { class: "scroller", id: "table", role: "table", "aria-label": "Items" }, this.spacer, this.body);
    scroller.addEventListener("scroll", () => this.draw(scroller));
    new ResizeObserver(() => this.draw(scroller)).observe(scroller);
    this.el.append(filters, this.summary, this.notice, h("div", { class: "table" }, this.header, scroller));
    this.scroller = scroller;
    this.categorySelect = category;
  }

  private renderHeader(): void {
    clear(this.header);
    for (const c of COLUMNS) {
      const active = this.sort.key === c.key || (c.key === "last_sold" && this.sort.key === "days");
      const arrow = active ? (this.sort.desc ? " ↓" : " ↑") : "";
      this.header.append(
        h(
          "button",
          {
            class: `th ${c.cls ?? ""} col-${c.key}`,
            role: "columnheader",
            "aria-sort": active ? (this.sort.desc ? "descending" : "ascending") : "none",
            onclick: () => {
              const key = c.key === "last_sold" ? "days" : c.key;
              this.sort = { key, desc: this.sort.key === key ? !this.sort.desc : key === "days" || key === "value" };
              this.renderHeader();
              this.arrange();
            },
          },
          c.title + arrow,
        ),
      );
    }
  }

  async load(): Promise<void> {
    try {
      this.report = await api.report(this.query);
    } catch (e) {
      this.notice.hidden = false;
      this.notice.textContent = `Couldn't build the report: ${String(e)}`;
      return;
    }
    this.notice.hidden = true;
    const category = this.categorySelect;
    const current = this.query.category ?? "";
    clear(category);
    category.append(h("option", { value: "" }, "All categories"));
    for (const c of this.report.categories) category.append(h("option", { value: c, selected: c === current }, c));
    this.arrange();
  }

  private arrange(): void {
    const report = this.report;
    if (!report) return;
    const term = this.search;
    this.rows = report.rows.filter(
      (r) =>
        !term ||
        r.item.toLowerCase().includes(term) ||
        r.variation.toLowerCase().includes(term) ||
        (r.sku ?? "").toLowerCase().includes(term),
    );
    const { key, desc } = this.sort;
    this.rows.sort((a, b) => (desc ? -compare(a, b, key) : compare(a, b, key)) || a.item.localeCompare(b.item));
    const slow = this.rows.filter((r) => r.slow);
    const atPrice = slow.reduce((s, r) => s + (r.value_at_price ?? 0), 0);
    const atCost = slow.reduce((s, r) => s + (r.value_at_cost ?? 0), 0);
    const currency = this.rows.find((r) => r.currency)?.currency ?? "USD";
    const items = new Set(slow.map((r) => r.item_id)).size;
    this.summary.textContent =
      slow.length === 0
        ? `Nothing has gone ${report.window_days} days without a sale.`
        : `${plural(slow.length, "slow variation")} across ${plural(items, "item")} · ${money(atPrice, currency)} tied up at price` +
          (atCost > 0 ? ` (${money(atCost, currency)} at cost)` : "");
    const scroller = this.scroller;
    this.spacer.style.height = `${this.rows.length * ROW_HEIGHT}px`;
    scroller.scrollTop = Math.min(scroller.scrollTop, Math.max(0, this.rows.length * ROW_HEIGHT - scroller.clientHeight));
    this.draw(scroller);
  }

  private draw(scroller: HTMLElement): void {
    const first = Math.max(0, Math.floor(scroller.scrollTop / ROW_HEIGHT) - 10);
    const count = Math.ceil(scroller.clientHeight / ROW_HEIGHT) + 20;
    const last = Math.min(this.rows.length, first + count);
    clear(this.body);
    this.body.style.transform = `translateY(${first * ROW_HEIGHT}px)`;
    if (this.rows.length === 0) {
      this.body.style.transform = "";
      this.body.append(h("div", { class: "empty" }, this.report ? "No items match." : "Loading…"));
      return;
    }
    for (let i = first; i < last; i++) this.body.append(this.row(this.rows[i]));
  }

  private row(r: ReportRow): HTMLElement {
    const value =
      r.value_at_price === null
        ? h("span", { class: "muted" }, r.on_hand === null ? "" : "—")
        : h(
            "span",
            {},
            money(r.value_at_price, r.currency),
            r.value_at_cost !== null ? h("small", {}, `cost ${money(r.value_at_cost, r.currency)}`) : null,
          );
    const label = r.label_status;
    const labelClass = label === "In Square" ? "ok" : label.startsWith("Skipped") ? "warn" : "muted";
    return h(
      "div",
      { class: `tr ${r.slow ? "slow" : ""}`, role: "row", "data-variation": r.variation_id },
      h("span", { class: "td col-item", role: "cell", title: r.item }, r.item),
      h("span", { class: "td col-variation", role: "cell", title: r.sku ? `SKU ${r.sku}` : r.variation }, r.variation),
      h("span", { class: "td col-category muted", role: "cell" }, r.category ?? ""),
      h("span", { class: "td col-last_sold", role: "cell" }, r.slow ? h("b", { class: "tag" }, "SLOW") : null, r.last_sold),
      h("span", { class: "td num col-days", role: "cell" }, r.days_since === null ? "—" : r.days_since.toLocaleString()),
      h("span", { class: "td num col-on_hand", role: "cell" }, quantity(r.on_hand)),
      h("span", { class: "td num col-value", role: "cell" }, ...(value.childNodes.length ? [...value.childNodes] : [value])),
      h("span", { class: `td col-label ${labelClass}`, role: "cell", title: label }, label),
    );
  }

  private async save(format: "xlsx" | "csv"): Promise<void> {
    const menu = this.el.querySelector("#save-menu") as HTMLElement;
    menu.hidden = true;
    try {
      const path = await api.exportReport(this.query, format);
      if (path) this.flash(`Saved to ${path}`);
    } catch (e) {
      this.flash(`Couldn't save: ${String(e)}`);
    }
  }

  private flash(text: string): void {
    this.notice.hidden = false;
    this.notice.textContent = text;
    window.setTimeout(() => {
      this.notice.hidden = true;
    }, 6000);
  }
}
