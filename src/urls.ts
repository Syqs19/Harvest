// Modalità bulk: l'utente incolla un link di esempio reale (es. la pagina 1),
// l'app trova i numeri contenuti nell'URL e genera la serie facendo variare
// quello scelto dall'utente.

export interface NumberMatch {
  start: number; // posizione nel testo dell'URL
  text: string; // il numero così come appare (es. "01")
}

export function findNumbers(url: string): NumberMatch[] {
  const matches: NumberMatch[] = [];
  for (const m of url.matchAll(/\d+/g)) {
    matches.push({ start: m.index, text: m[0] });
  }
  return matches;
}

/**
 * Tetto ai link generati in una volta. Serve contro gli errori di battitura
 * (un "fino a 100000" genererebbe centomila schede e bloccherebbe l'app), non
 * a limitare l'uso reale: 500 download in coda sono già ore di lavoro.
 */
export const MAX_SERIES = 500;

/** Quanti link genererebbe questo intervallo (anche se supera il tetto). */
export function seriesCount(from: number, to: number): number {
  if (!Number.isInteger(from) || !Number.isInteger(to) || from > to) return 0;
  return to - from + 1;
}

export function generateSeriesUrls(
  exampleUrl: string,
  match: NumberMatch,
  from: number,
  to: number,
  zeroPad: boolean,
): string[] {
  if (!Number.isInteger(from) || !Number.isInteger(to) || from > to) return [];
  // Oltre il tetto non genero nulla: la UI mostra un avviso al posto
  // dell'anteprima. Meglio niente che un'app bloccata.
  if (seriesCount(from, to) > MAX_SERIES) return [];

  const before = exampleUrl.slice(0, match.start);
  const after = exampleUrl.slice(match.start + match.text.length);
  // Con gli zeri iniziali si mantiene la larghezza del numero d'esempio (es. "01" -> 2 cifre),
  // allargandola se il range richiede più cifre (es. fino a 120 -> 3 cifre)
  const width = zeroPad ? Math.max(2, match.text.length, String(to).length) : 0;

  const urls: string[] = [];
  for (let n = from; n <= to; n++) {
    urls.push(before + String(n).padStart(width, "0") + after);
  }
  return urls;
}
