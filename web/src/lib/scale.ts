/**
 * A steady y-axis for the hashrate chart. The top is a "nice" value from the ladder
 * 1, 2, 2.5, 5 × 10ⁿ (in H/s), so tick labels land on round numbers, and it only moves
 * when the data has clearly outgrown it or clearly shrunk below it, so a live estimate
 * wobbling around a rung does not rescale the chart every snapshot.
 */

const MANTISSAS = [1, 2, 2.5, 5] as const;
/** Tick count for each mantissa, so every tick is round: 0.25, 0.5, 0.5, 1 × 10ⁿ apart. */
const TICKS = [4, 4, 5, 5] as const;

/** The top must clear the data max by this factor. */
export const HEADROOM = 1.1;
/**
 * The top steps down only once the data (with headroom) fits under this fraction of the
 * rung below the current top.
 */
export const STEP_DOWN = 0.8;

/** Relative tolerance for comparing against rungs, which are not all exact in binary. */
const EPS = 1e-9;

/** Rung `k` of the ladder: k = 4n + i is MANTISSAS[i] × 10ⁿ. */
function rung(k: number): number {
  const n = Math.floor(k / 4);
  const i = k - n * 4;
  return MANTISSAS[i] * 10 ** n;
}

/** The index of the smallest rung that is at least `v` (v > 0). */
function rungIndex(v: number): number {
  let k = 4 * Math.floor(Math.log10(v)) - 1;
  while (rung(k) < v * (1 - EPS)) k += 1;
  return k;
}

function positive(v: number): boolean {
  return Number.isFinite(v) && v > 0;
}

/** The smallest ladder value at least `v`; 1 for zero, negative, or non-finite input. */
export function niceCeil(v: number): number {
  return positive(v) ? rung(rungIndex(v)) : 1;
}

/** The ladder value just below `top` (itself a ladder value). */
export function rungBelow(top: number): number {
  return rung(rungIndex(top) - 1);
}

/**
 * The chart's top for a data max (the largest sample or live estimate), given the top
 * currently shown (`previous`, absent on first draw). Steps up as soon as the data with
 * headroom no longer fits; steps down only when it fits under `STEP_DOWN` of the rung
 * below, so a value hovering at a boundary keeps one scale.
 */
export function chartTop(dataMax: number, previous?: number | null): number {
  const need = (positive(dataMax) ? dataMax : 0) * HEADROOM;
  const fresh = niceCeil(need);
  if (previous == null || !positive(previous)) return fresh;
  if (fresh >= previous * (1 - EPS)) return fresh;
  return need <= rungBelow(previous) * STEP_DOWN ? fresh : previous;
}

/** Tick values from the first tick up to `top` (a ladder value), all round numbers. */
export function axisTicks(top: number): number[] {
  if (!positive(top)) return [];
  const k = rungIndex(top);
  const n = TICKS[k - Math.floor(k / 4) * 4];
  return Array.from({ length: n }, (_, i) => (top * (i + 1)) / n);
}
