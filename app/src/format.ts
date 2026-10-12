// Times, money and quantities as the owner reads them.

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** "5:00 a.m." */
export function clock(hour: number, minute: number): string {
  const suffix = hour < 12 ? "a.m." : "p.m.";
  const h12 = hour % 12 === 0 ? 12 : hour % 12;
  return `${h12}:${String(minute).padStart(2, "0")} ${suffix}`;
}

function sameDay(a: Date, b: Date): boolean {
  return a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
}

/** "today 5:02 a.m.", "tomorrow 5:00 a.m.", "yesterday 9:14 p.m.", "Oct 9, 5:02 a.m." */
export function when(iso: string | null, now = new Date()): string {
  if (!iso) return "never";
  const t = new Date(iso);
  const time = clock(t.getHours(), t.getMinutes());
  const tomorrow = new Date(now);
  tomorrow.setDate(now.getDate() + 1);
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (sameDay(t, now)) return `today ${time}`;
  if (sameDay(t, tomorrow)) return `tomorrow ${time}`;
  if (sameDay(t, yesterday)) return `yesterday ${time}`;
  return `${MONTHS[t.getMonth()]} ${t.getDate()}, ${time}`;
}

export function money(cents: number | null, currency: string | null): string {
  if (cents === null) return "";
  try {
    const locale = navigator.language && navigator.language !== "C" ? navigator.language : "en-US";
    return new Intl.NumberFormat(locale, { style: "currency", currency: currency || "USD" }).format(cents / 100);
  } catch {
    return (cents / 100).toFixed(2);
  }
}

export function quantity(q: number | null): string {
  if (q === null) return "";
  return Number.isInteger(q) ? String(q) : q.toFixed(2).replace(/0+$/, "").replace(/\.$/, "");
}

export function plural(n: number, one: string, many = one + "s"): string {
  return `${n.toLocaleString()} ${n === 1 ? one : many}`;
}
