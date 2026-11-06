/** The numeric parts of a moment in `timeZone`, with the hour on a 24-hour clock. */
function parts(moment: number, timeZone: string | undefined): Record<string, string> {
  const formatted = new Intl.DateTimeFormat("en-US", {
    timeZone,
    hourCycle: "h23",
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  }).formatToParts(moment);
  return Object.fromEntries(formatted.map((part) => [part.type, part.value]));
}

/**
 * An absolute time, which the window shows where an age would drift: the time of day when it is the same day as
 * `now`, the date before it otherwise. `timeZone` is the zone of the machine unless a test says otherwise.
 */
export function formatMoment(moment: number, now: number, timeZone?: string): string {
  const at = parts(moment, timeZone);
  const today = parts(now, timeZone);
  const time = `${at.hour}:${at.minute}:${at.second}`;
  const sameDay = at.year === today.year && at.month === today.month && at.day === today.day;
  return sameDay ? time : `${at.year}-${at.month}-${at.day} ${time}`;
}

/** Like `formatMoment`, to the minute, for a line too narrow to hold the seconds. */
export function formatMinute(moment: number, now: number, timeZone?: string): string {
  const at = parts(moment, timeZone);
  const today = parts(now, timeZone);
  const time = `${at.hour}:${at.minute}`;
  const sameDay = at.year === today.year && at.month === today.month && at.day === today.day;
  return sameDay ? time : `${at.year}-${at.month}-${at.day} ${time}`;
}
