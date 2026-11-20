export type CadenceKind = 'daily' | 'interval' | 'cron' | 'once';

export interface ParsedRule {
  kind: CadenceKind;
  isLocal: boolean;
  dailyTime?: string; // "HH:MM"
  intervalSecs?: number;
  cronExpr?: string;
  onceIso?: string;
  raw: string;
}

export function getDeviceTimezone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || 'Device local';
  } catch {
    return 'Device local';
  }
}

export function formatTime12h(time24: string): string {
  const parts = time24.split(':');
  if (parts.length < 2) return time24;
  const hours = parseInt(parts[0], 10);
  const minutes = parts[1].padStart(2, '0');
  if (isNaN(hours)) return time24;

  const period = hours >= 12 ? 'PM' : 'AM';
  const displayHours = hours % 12 === 0 ? 12 : hours % 12;
  return `${displayHours}:${minutes} ${period}`;
}

export function parseScheduleRule(ruleStr: string): ParsedRule {
  const raw = (ruleStr || '').trim();

  if (raw.startsWith('daily-local:')) {
    const dailyTime = raw.slice(12).trim();
    return {
      kind: 'daily',
      isLocal: true,
      dailyTime,
      raw,
    };
  }

  if (raw.startsWith('daily:')) {
    const dailyTime = raw.slice(6).trim();
    return {
      kind: 'daily',
      isLocal: false,
      dailyTime,
      raw,
    };
  }

  if (raw.startsWith('every:')) {
    const secs = parseInt(raw.slice(6).trim(), 10);
    return {
      kind: 'interval',
      isLocal: false,
      intervalSecs: isNaN(secs) ? 60 : secs,
      raw,
    };
  }

  if (raw.startsWith('local-cron:')) {
    const cronExpr = raw.slice(11).trim();
    return {
      kind: 'cron',
      isLocal: true,
      cronExpr,
      raw,
    };
  }

  if (raw.startsWith('once:')) {
    const onceIso = raw.slice(5).trim();
    return {
      kind: 'once',
      isLocal: false,
      onceIso,
      raw,
    };
  }

  // Check 5-field cron
  const parts = raw.split(/\s+/).filter(Boolean);
  if (parts.length === 5) {
    return {
      kind: 'cron',
      isLocal: false,
      cronExpr: raw,
      raw,
    };
  }

  return {
    kind: 'interval',
    isLocal: false,
    intervalSecs: 60,
    raw,
  };
}

export function formatScheduleRule(parsed: {
  kind: CadenceKind;
  isLocal?: boolean;
  dailyTime?: string;
  intervalSecs?: number;
  cronExpr?: string;
  onceIso?: string;
}): string {
  if (parsed.kind === 'daily') {
    const time = (parsed.dailyTime || '09:00').trim();
    if (parsed.isLocal !== false) {
      return `daily-local:${time}`;
    }
    return `daily:${time}`;
  }

  if (parsed.kind === 'interval') {
    const secs = Math.max(10, parsed.intervalSecs ?? 60);
    return `every:${secs}`;
  }

  if (parsed.kind === 'cron') {
    const expr = (parsed.cronExpr || '0 9 * * *').trim();
    if (parsed.isLocal !== false) {
      return `local-cron:${expr}`;
    }
    return expr;
  }

  if (parsed.kind === 'once') {
    return `once:${(parsed.onceIso || '').trim()}`;
  }

  return 'daily-local:09:00';
}

export function describeScheduleRule(ruleStr: string): {
  summary: string;
  detail: string;
  isLocal: boolean;
} {
  const parsed = parseScheduleRule(ruleStr);

  if (parsed.kind === 'daily') {
    const time12 = formatTime12h(parsed.dailyTime || '09:00');
    if (parsed.isLocal) {
      return {
        summary: `Daily at ${time12}`,
        detail: `Host device local time (${getDeviceTimezone()})`,
        isLocal: true,
      };
    }
    return {
      summary: `Daily at ${parsed.dailyTime || '09:00'} UTC`,
      detail: 'Legacy UTC schedule',
      isLocal: false,
    };
  }

  if (parsed.kind === 'cron') {
    if (parsed.isLocal) {
      return {
        summary: `Cron: ${parsed.cronExpr}`,
        detail: `Host device local time (${getDeviceTimezone()})`,
        isLocal: true,
      };
    }
    return {
      summary: `Cron: ${parsed.cronExpr} (UTC)`,
      detail: 'Evaluated in UTC',
      isLocal: false,
    };
  }

  if (parsed.kind === 'interval') {
    const secs = parsed.intervalSecs || 60;
    let text = `${secs}s`;
    if (secs >= 86400 && secs % 86400 === 0) {
      const d = secs / 86400;
      text = `${d} day${d === 1 ? '' : 's'}`;
    } else if (secs >= 3600 && secs % 3600 === 0) {
      const h = secs / 3600;
      text = `${h} hour${h === 1 ? '' : 's'}`;
    } else if (secs >= 60 && secs % 60 === 0) {
      const m = secs / 60;
      text = `${m} minute${m === 1 ? '' : 's'}`;
    } else if (secs >= 60) {
      const m = Math.floor(secs / 60);
      const s = secs % 60;
      text = s > 0 ? `${m}m ${s}s` : `${m}m`;
    }
    return {
      summary: `Every ${text}`,
      detail: 'Fixed interval',
      isLocal: false,
    };
  }

  if (parsed.kind === 'once') {
    return {
      summary: 'One-time execution',
      detail: parsed.onceIso || 'Scheduled timestamp',
      isLocal: false,
    };
  }

  return {
    summary: parsed.raw,
    detail: 'Custom rule',
    isLocal: false,
  };
}
