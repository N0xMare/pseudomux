# The local-command exercise: what it should measure, and what it should not

**Status: NOT EXECUTED.** No part of this document has been run. It is a plan
and a scoping argument, not evidence. Everything below that says MEASURED is
cited from the existing study; everything else is untested.

## The exercise is much smaller than "85 commands"

The original framing was: 85 of Claude Code 2.1.220's 86 slash commands are
local and call no model, so the command surface can be exercised in bulk at zero
model cost — open the menu at every prefix, cancel, resize, submit, interrupt,
and diff pmux's classification against the transcript oracle.

That is true about Claude Code and mostly irrelevant to pmux, because of one
fact in `crates/service/src/driver_io.rs`:

```rust
enum ControlCommand {
    Clear,
}
```

`ControlCommand` is a single-variant enum carrying no payload, and its `literal`
is selected at compile time from that file. **`/clear` is the only slash command
pmux can ever type.** A caller cannot reach the others either: `validate_prompt`
refuses any prompt whose first non-format character is a solidus, and
`crates/service/tests/paste_injection.rs::a_caller_can_never_type_a_slash_command`
holds that shut.

So "diff pmux's classification across 85 commands" is diffing a classification
pmux will never perform. Running it would produce a large, real, and almost
entirely inert result.

## The question that IS worth the wall-clock

The menu is still a risk, but a narrow one. MEASURED, and already noted in
`driver_io.rs`: *the commands that sort next to `/clear` in the menu* are the
ones a mis-selected Enter could execute instead. The selection is rendered in
FOREGROUND COLOUR ALONE, which is why `StyledScreen` exists at all.

So the exercise worth running is not 85 commands wide. It is:

1. Open the menu at every prefix of `/clear` — `/`, `/c`, `/cl`, `/cle`,
   `/clea`, `/clear` — and at the prefixes of its menu NEIGHBOURS.
2. At each, capture a `StyledScreen` and record it into the corpus.
3. Assert `prove_control_command_selection` picks `/clear` and nothing else, and
   that the highlighted row is the one whose text is `/clear`.
4. Repeat across pane sizes, because the menu reflows.

That is a handful of prefixes, not 86, and every frame it captures feeds the
same standing invariants as any other corpus recording.

## Commands that must never be executed

Open-and-cancel (ESC) only. Never Enter. These mutate operator state and are not
recoverable by the harness:

    /logout  /login  /config  /exit  /quit  /upgrade  /install-github-app
    /install-slack-app  /permissions  /mcp  /plugin  /hooks  /model
    /privacy-settings  /statusline  /output-style  /terminal-setup  /vim
    /migrate-installer  /doctor  /bug  /feedback

The safe pattern is: type the prefix, capture, send ESC, confirm the composer
returned to the empty geometry, and only then move on. `/clear` is the single
exception, and only because pmux already submits it deliberately with a
transcript-rotation proof behind it.

## What I could not enumerate offline

Claude Code 2.1.220 ships as a Bun-compiled Mach-O binary with no
machine-readable command list, and `claude --help` does not carry one. Pulling
candidates out of the binary's string table does not work: the results
interleave real commands with URL paths, loader paths and Bun internals
(`/proc`, `/ld-musl-`, `/actions-runner`, `/jsx-dev-runtime`), with no reliable
way to tell them apart. An accurate list therefore requires driving the TUI's
own menu, which requires the live stack. That is why the denylist above is
hand-written and conservative rather than derived, and why it should be treated
as incomplete: **open-and-cancel everything, Enter only `/clear`.**
