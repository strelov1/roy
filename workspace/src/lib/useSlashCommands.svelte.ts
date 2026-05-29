// Slash-command popover machinery shared by Composer.svelte and
// ChatView.svelte. Both composers drive an identical popover off a
// relative-positioned <textarea>; the only differences are the agent source
// (a local `agent` vs `app.currentAgent`) and the autosize cap, which the
// caller supplies via the accessors below.
//
// Triggers when the caret sits in a token that starts with `/` *at* the start
// of the draft or right after whitespace — matches the convention agents
// already use for slash commands. Closes when the user types a space,
// backspaces past the `/`, navigates away, or hits Escape.

import { supportsSlashCommands, type Harness } from './wire';
import { fetchCommandBody } from './commands.svelte';
import { focusCaret, spliceAtCaret } from './utils';

export type SlashCtx = { start: number; query: string };

export interface SlashCommandsOpts {
  getTextarea: () => HTMLTextAreaElement | undefined;
  getAgent: () => Harness | '' | null | undefined;
  getDraft: () => string;
  setDraft: (v: string) => void;
}

export function useSlashCommands(opts: SlashCommandsOpts) {
  const { getTextarea, getAgent, getDraft, setDraft } = opts;

  let slash = $state<SlashCtx | null>(null);

  function closeSlash() {
    if (slash !== null) slash = null;
  }

  function refreshSlash() {
    const textareaEl = getTextarea();
    if (!textareaEl || !supportsSlashCommands(getAgent())) {
      closeSlash();
      return;
    }
    const value = textareaEl.value;
    const caret = textareaEl.selectionStart ?? value.length;
    // Walk back from the caret until we hit whitespace or the start of the
    // textarea; the candidate `/` is the first char of that token.
    let i = caret;
    while (i > 0 && !/\s/.test(value[i - 1]!)) i -= 1;
    if (value[i] !== '/') {
      closeSlash();
      return;
    }
    slash = { start: i, query: value.slice(i + 1, caret) };
  }

  function insertAtCaret(text: string) {
    const textareaEl = getTextarea();
    if (!textareaEl) return;
    const { next, caret } = spliceAtCaret(textareaEl, getDraft(), text);
    setDraft(next);
    focusCaret(textareaEl, caret);
  }

  function openSlashFromMenu() {
    const textareaEl = getTextarea();
    if (!textareaEl) return;
    const draft = getDraft();
    const slashStart = textareaEl.selectionStart ?? draft.length;
    const { next, caret } = spliceAtCaret(textareaEl, draft, '/');
    setDraft(next);
    slash = { start: slashStart, query: '' };
    focusCaret(textareaEl, caret);
  }

  async function pickCommand(name: string) {
    let textareaEl = getTextarea();
    if (!textareaEl || !slash) return;
    // Fetch the skill's body so we can splice it into the draft instead of
    // the literal `/<name>`. This is what makes commands work across all
    // harnesses (Claude / Codex / Gemini / OpenCode): every agent receives
    // the unfolded instructions as part of the prompt.
    //
    // Close the popover immediately so the textarea doesn't show a stale
    // list while the fetch is in flight.
    const cap = slash;
    closeSlash();
    const body = await fetchCommandBody(name);
    textareaEl = getTextarea();
    if (!textareaEl) return;
    const fallback = `/${name} `;
    const insert = (body ?? fallback).trimEnd() + '\n\n';
    const draft = getDraft();
    const before = draft.slice(0, cap.start);
    const after = draft.slice(cap.start + 1 + cap.query.length);
    const next = before + insert + after;
    setDraft(next);
    const newCaret = before.length + insert.length;
    focusCaret(textareaEl, newCaret);
  }

  return {
    get slash() {
      return slash;
    },
    refreshSlash,
    closeSlash,
    insertAtCaret,
    openSlashFromMenu,
    pickCommand,
  };
}
