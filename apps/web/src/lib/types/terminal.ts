// Wire shapes for the WS terminal stream (`GET /api/sessions/:id/term`). Two
// frame styles coexist over a single upgraded socket:
//
//   • `Message::Binary` — raw pane bytes. Fast path. The dashboard pipes them
//     straight into the xterm instance without inspection. This is the default
//     producer/consumer flow today (hq-fe-term.2).
//
//   • `Message::Text` JSON — control envelope. Fields are independent + optional;
//     a frame may carry one or both. Existing verbs:
//       - `resize` (hq-fe-term.2): `{cols, rows}` advisory geometry hint.
//       - `chunk`  (hq-fe-term.3): typed annotation the dashboard renders with
//         per-kind styling.
//     Future verbs land as additional fields on the envelope without breaking
//     older peers — serde-untagged on the Rust side, optional on the TS side.

/** Kind tag for typed terminal chunks. Mirrors `gt_web::dto::TerminalChunkKind`
 *  1:1; the dashboard renders each kind with distinct styling (raw passthrough,
 *  code monospace, comment muted, highlight emphasised, warn amber). */
export type TerminalChunkKind = 'raw' | 'code' | 'comment' | 'highlight' | 'warn';

/** One typed chunk (hq-fe-term.3). `text` is the rendered string the producer
 *  asserts about; the server does not strip or re-encode terminal escape
 *  sequences. Producers are responsible for the byte content. */
export interface TerminalChunk {
  kind: TerminalChunkKind;
  text: string;
}

/** Resize advisory (hq-fe-term.2). Adapters that cannot resize (fake driver, pty
 *  without SIGWINCH support) silently ignore the hint. */
export interface TerminalResize {
  cols: number;
  rows: number;
}

/** Control envelope for the WS text channel. Carries any combination of the
 *  declared verbs; unknown peers extend the union with additional optional
 *  fields when new verbs ship. */
export interface TerminalStreamFrame {
  resize?: TerminalResize;
  chunk?: TerminalChunk;
}
