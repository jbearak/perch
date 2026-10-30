// SPDX-License-Identifier: GPL-3.0-only

// `perch doctor --json` and the one-time warning the extension shows when
// sessions on a host are not expected to survive logout.

export interface DoctorCheck {
  name: string;
  status: string;
  message: string;
}

export interface DoctorReport {
  ok: boolean;
  /** `null` when Perch could not tell. */
  survivesLogout: boolean | null;
  runtimeDir: string;
  checks: DoctorCheck[];
}

/** Parses `perch doctor --json`. Throws on anything but schema 1. */
export function parseDoctor(stdout: string): DoctorReport {
  const doc: unknown = JSON.parse(stdout);
  if (typeof doc !== 'object' || doc === null || (doc as { schema?: unknown }).schema !== 1) {
    throw new Error('perch doctor --json: unexpected output');
  }
  const d = doc as Record<string, unknown>;
  const checks = Array.isArray(d.checks) ? d.checks : [];
  return {
    ok: d.ok !== false,
    survivesLogout: typeof d.survivesLogout === 'boolean' ? d.survivesLogout : null,
    runtimeDir: typeof d.runtimeDir === 'string' ? d.runtimeDir : '',
    checks: checks
      .filter((c): c is Record<string, unknown> => typeof c === 'object' && c !== null)
      .map((c) => ({ name: String(c.name ?? ''), status: String(c.status ?? ''), message: String(c.message ?? '') })),
  };
}

/**
 * The warning for this host, or undefined: sessions are not expected to
 * survive logout, or a check failed (an NFS runtime directory without
 * `flock`, for example). Undecided hosts get none.
 */
export function doctorWarning(r: DoctorReport, host: string): string | undefined {
  if (r.survivesLogout !== false && r.ok) return undefined;
  const problems = r.checks.filter((c) => c.status === 'warn' || c.status === 'fail').map((c) => c.message);
  const head =
    r.survivesLogout === false
      ? `Perch sessions on ${host} are not expected to survive logout.`
      : `Perch found a problem on ${host}.`;
  const why = problems.length > 0 ? ` ${problems.map((p) => (/[.!?]$/.test(p) ? p : `${p}.`)).join(' ')}` : '';
  return `${head}${why} Run \`perch doctor\` for details.`;
}

/** The `globalState` key that records the warning was shown for a host. */
export function doctorWarnedKey(host: string): string {
  return `perch.doctorWarned.${host}`;
}
