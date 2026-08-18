"""The README's documented surface, DERIVED from the things that ship it.

`README.md` published a command surface of ten subcommands while the binary
offered thirteen, and `pmux ask` -- the entire provider product -- appeared in
it zero times. The MCP section named eight tools while the server answered
`tools/list` with thirteen. Neither drift was reachable by any check: the
README's lists were prose, and prose is a claim.

So every list in this module is asked of the artefact and never restated here:

* the published subcommand set and each one's `API` / `Ops` label come from
  `pmux --help`, cross-checked against the non-hidden `Command` variants in
  `bin/pmux/src/cli.rs` so a stale binary cannot quietly agree with a stale
  README;
* the pool daemon flags come from `pmuxd serve --help`;
* the pool cap comes from `MAX_POOL_SIZE`;
* the model table comes from `MODEL_TABLE`;
* the MCP tool list comes from a real `tools/list` exchange with `pmux-mcp`;
* the flags the quickstart's daemon must carry come from the refusal a caller
  actually hits when it does not (`pool::refusal::path_b_not_enabled`).

These tests read the tracked tree and run product binaries with `--help` or on
stdin. None starts a daemon, opens a socket, launches Claude or spends a token.
They need a built workspace: `cargo build --workspace --release`, which Gate A
requires anyway -- the driver refuses a missing or stale release directory
before any cell runs.
"""

from __future__ import annotations

import json
import pathlib
import re
import subprocess
import unittest

WORKSPACE = pathlib.Path(__file__).resolve().parents[3]
README = WORKSPACE / "README.md"

# The clap-generated pseudo-subcommand. It is not part of the product surface
# and no README should be asked to document it.
CLAP_BUILTIN_SUBCOMMANDS = frozenset({"help"})


def binary(name: str) -> pathlib.Path:
    """The built product binary, release first.

    Release first because that is the one Gate A validates and freezes; debug is
    the ordinary development build and is accepted so this suite still runs
    between `cargo build` and `cargo build --release`. Whichever is found is
    checked against the source tree below, so a stale one fails loudly rather
    than agreeing with a stale README.
    """

    candidates = [
        WORKSPACE / "target" / profile / name for profile in ("release", "debug")
    ]
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    raise AssertionError(
        f"no built {name}; looked at {[str(path) for path in candidates]}. "
        f"Run `cargo build --workspace --release`."
    )


def run(argv: list[str], stdin: str | None = None) -> str:
    completed = subprocess.run(
        argv,
        input=stdin,
        capture_output=True,
        text=True,
        check=False,
        cwd=WORKSPACE,
        timeout=120,
    )
    if completed.returncode != 0:
        raise AssertionError(
            f"{argv} exited {completed.returncode}: {completed.stderr.strip()}"
        )
    return completed.stdout


def help_subcommands(text: str) -> dict[str, str]:
    """`{name: one-line description}` from a clap `Commands:` block.

    A new entry starts at exactly two spaces of indent; clap wraps a long
    description onto lines indented further, and those are folded back in rather
    than parsed as commands.
    """

    _, _, after = text.partition("\nCommands:\n")
    commands: dict[str, str] = {}
    last: str | None = None
    for line in after.splitlines():
        if not line.strip():
            if commands:
                break
            continue
        start = re.match(r"^ {2}(\S+) {2,}(.*)$", line)
        if start:
            last = start.group(1)
            commands[last] = start.group(2).strip()
        elif last is not None and line.startswith("    "):
            commands[last] = f"{commands[last]} {line.strip()}"
        else:
            break
    return commands


def kebab(variant: str) -> str:
    """clap's default subcommand spelling for an enum variant name."""

    return re.sub(r"(?<!^)(?=[A-Z])", "-", variant).lower()


def rust_block(source: str, opener: str) -> str:
    """The body of a braced item, from `opener` to the matching column-0 `}`."""

    _, marker, after = source.partition(opener)
    if not marker:
        raise AssertionError(f"{opener!r} is gone from the source")
    body: list[str] = []
    for line in after.splitlines():
        if line.startswith("}"):
            return "\n".join(body)
        body.append(line)
    raise AssertionError(f"{opener!r} has no closing brace at column zero")


def markdown_tables(text: str) -> list[list[list[str]]]:
    """Every GitHub-flavoured table, as a list of rows of stripped cells."""

    tables: list[list[list[str]]] = []
    current: list[list[str]] = []
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("|") and stripped.endswith("|"):
            current.append([cell.strip() for cell in stripped.strip("|").split("|")])
            continue
        if current:
            tables.append(current)
            current = []
    if current:
        tables.append(current)
    return [table for table in tables if len(table) >= 3]


def table_with_header(text: str, *header: str) -> list[list[str]]:
    """The one table whose leading header cells are exactly `header`.

    Exactly one, asserted: two tables with the same head would mean the check
    silently picked whichever came first.
    """

    width = len(header)
    matches = [
        table
        for table in markdown_tables(text)
        if [cell.lower() for cell in table[0][:width]]
        == [cell.lower() for cell in header]
    ]
    if len(matches) != 1:
        raise AssertionError(
            f"README.md has {len(matches)} tables headed {list(header)}; expected exactly one"
        )
    # Row 1 is the `| --- |` separator.
    return matches[0][2:]


def backticked(text: str) -> list[str]:
    return re.findall(r"`([^`]+)`", text)


def flatten(text: str) -> str:
    """One line, with doc-comment markers gone.

    A phrase this module looks for is written by a human and then wrapped by
    `rustfmt` or by a fill column, so it is never reliably on one line: the pool
    cap is spelled `owner-set cap of\\n/// 15` in `bin/pmuxd/src/main.rs`. This
    is the same flattening `test_run_gate.py`'s bug-class counter does, and for
    the same reason -- a scan that a line break can defeat reports agreement
    over the sites it happened to still find.
    """

    return re.sub(
        r"\s+",
        " ",
        "\n".join(line.lstrip().lstrip("/!").strip() for line in text.splitlines()),
    )


class DocumentedSurfaceTest(unittest.TestCase):
    def readme(self) -> str:
        return README.read_text(encoding="utf-8")

    # -- the CLI ---------------------------------------------------------

    def test_the_readme_command_table_is_the_binarys_own_subcommand_list(self):
        """Two derivations, then the document -- in that order.

        The binary is the authority for what ships. `bin/pmux/src/cli.rs` is
        checked against it FIRST, for two reasons: it makes a stale build fail
        here instead of blessing a stale README, and two independent
        derivations agreeing is what makes this test non-vacuous. A `Commands:`
        parse that silently returned nothing would otherwise "agree" with a
        README table that had also been emptied.
        """

        pmux = binary("pmux")
        described = help_subcommands(run([str(pmux), "--help"]))
        offered = set(described) - CLAP_BUILTIN_SUBCOMMANDS
        self.assertTrue(offered, f"parsed no subcommand out of `{pmux.name} --help`")

        source = (WORKSPACE / "bin" / "pmux" / "src" / "cli.rs").read_text(
            encoding="utf-8"
        )
        block = rust_block(source, "\npub enum Command {\n")
        self.assertNotRegex(
            block,
            r"#\[command\([^)]*\bname\s*=",
            "a Command variant renames itself with `#[command(name = ...)]`, so the "
            "kebab-case derivation below no longer produces its spelling",
        )
        declared: set[str] = set()
        visible: set[str] = set()
        pending_hide = False
        pending_attr = ""
        for line in block.splitlines():
            stripped = line.strip()
            if pending_attr or stripped.startswith("#["):
                pending_attr += stripped
                if stripped.endswith("]"):
                    if re.search(
                        r"#\[command\([^]]*hide\s*=\s*true", pending_attr
                    ):
                        pending_hide = True
                    pending_attr = ""
                continue
            match = re.match(r"^    ([A-Z][A-Za-z0-9]*)\s*[,{(]", line)
            if match:
                name = kebab(match.group(1))
                declared.add(name)
                if not pending_hide:
                    visible.add(name)
                pending_hide = False
        self.assertTrue(declared, "parsed no Command variant out of bin/pmux/src/cli.rs")
        hidden = declared - visible
        self.assertEqual(
            hidden,
            {
                "agent",
                "attach",
                "cancel",
                "clear",
                "close",
                "inspect",
                "oneshot",
                "probe",
                "start",
                "turn",
            },
            "the session CLI left the Command enum or lost hide; hide it, "
            "do not delete it",
        )
        self.assertEqual(
            offered,
            visible,
            f"{pmux} disagrees with bin/pmux/src/cli.rs about the published "
            f"subcommand set; the build is stale -- run `cargo build --workspace --release`",
        )

        # The label is the binary's own: everything before the first colon of
        # the one-line description. `pmux --help` promises that every published
        # subcommand says which surface it is, so a description with no colon
        # is that promise broken and not a parse to work around.
        labels = {}
        for name, description in described.items():
            if name in CLAP_BUILTIN_SUBCOMMANDS:
                continue
            label, colon, _ = description.partition(":")
            self.assertTrue(
                colon, f"`pmux {name}` states no surface label in its help summary"
            )
            labels[name] = label.strip()
        self.assertGreaterEqual(
            len(set(labels.values())),
            2,
            f"only {sorted(set(labels.values()))} came out of the help summaries; "
            f"pmux --help says there are two kinds of published subcommand, so a "
            f"smaller set means the label parse is broken",
        )

        # Order is deliberately NOT compared. The binary lists subcommands in
        # declaration order; the README leads with `run` because that is the
        # product, and forcing it to follow clap would be the document serving
        # the check rather than the reader.
        rows = table_with_header(self.readme(), "subcommand", "surface")
        documented = {}
        for row in rows:
            names = backticked(row[0])
            self.assertEqual(
                len(names), 1, f"README command-table row {row[0]!r} names {names}"
            )
            documented[names[0]] = row[1].strip()
        self.assertEqual(
            set(documented),
            offered,
            "README.md's command table is not the set of subcommands the binary "
            "offers; every subcommand must be documented and no row may name one "
            "that does not exist",
        )
        for name, label in sorted(documented.items()):
            with self.subTest(subcommand=name):
                self.assertEqual(
                    label,
                    labels[name],
                    f"README.md files `{name}` under {label!r} and `pmux --help` "
                    f"labels it {labels[name]!r}",
                )

    # -- the pool daemon --------------------------------------------------

    def test_the_readme_names_every_path_b_flag_the_daemon_offers(self):
        """`--path-b-*` is the whole configuration surface of the product.

        Derived from `pmuxd serve --help` rather than from the help heading
        that groups them, so a flag that loses its `help_heading` is still
        required to be documented.
        """

        text = run([str(binary("pmuxd")), "serve", "--help"])
        flags = set(re.findall(r"--path-b-[a-z0-9-]+", text))
        self.assertIn(
            "--path-b-parent",
            flags,
            "the scan lost the enable switch, so agreement over the rest says nothing",
        )
        readme = self.readme()
        for flag in sorted(flags):
            with self.subTest(flag=flag):
                # Bounded, not `in`: `--path-b-retain-dir` is a prefix of
                # `--path-b-retain-directory`, so a substring test calls a
                # README that documents a flag by the wrong name a pass.
                self.assertRegex(
                    readme,
                    re.escape(flag) + r"(?![a-z0-9-])",
                    f"README.md never mentions {flag}, which is part of how an "
                    f"operator sizes the stateless pool",
                )

    def test_every_statement_of_the_pool_cap_is_the_constant_the_daemon_enforces(self):
        """One number, currently written down in three places.

        `MAX_POOL_SIZE` is the predicate. `bin/pmuxd/src/main.rs` spells it into
        a doc comment that becomes `--help`, and the README spells it again.
        A cap raised in the constant and left alone in either sentence is two
        documents promising a bound the daemon does not enforce.
        """

        config = (
            WORKSPACE / "crates" / "service" / "src" / "pool" / "config.rs"
        ).read_text(encoding="utf-8")
        declared = re.search(r"pub const MAX_POOL_SIZE: u32 = (\d+);", config)
        self.assertIsNotNone(declared, "MAX_POOL_SIZE is gone from pool/config.rs")
        cap = declared.group(1)

        phrase = r"owner-set cap of (\d+)"
        sources = {
            "README.md": self.readme(),
            "bin/pmuxd/src/main.rs": (
                WORKSPACE / "bin" / "pmuxd" / "src" / "main.rs"
            ).read_text(encoding="utf-8"),
            "pmuxd serve --help": run([str(binary("pmuxd")), "serve", "--help"]),
        }
        for where, text in sorted(sources.items()):
            with self.subTest(source=where):
                stated = re.findall(phrase, flatten(text))
                self.assertTrue(
                    stated,
                    f"{where} no longer states the pool cap in the one shape this "
                    f"check reads ({phrase!r}), so it is unchecked",
                )
                self.assertEqual(
                    set(stated),
                    {cap},
                    f"{where} states a pool cap of {sorted(set(stated))}; "
                    f"MAX_POOL_SIZE is {cap}",
                )

    def test_the_readme_model_table_is_the_pools_own_model_table(self):
        """`--model` and `--effort` are half the product's contract each.

        Every cell is derived from `MODEL_TABLE`: the canonical spelling, the
        alias list, and the effort tiers -- resolved through the same
        `AdmittedEffort` constants the argv renderer draws from, so the words
        offered here are the words a launch can actually be rendered from.
        """

        source = (
            WORKSPACE / "crates" / "service" / "src" / "pool" / "class.rs"
        ).read_text(encoding="utf-8")
        # `const LOW: AdmittedEffort = AdmittedEffort { level: .., argv: "low" };`
        argv_of = dict(
            re.findall(
                r"const (\w+): AdmittedEffort = AdmittedEffort \{[^}]*argv: \"([a-z]+)\"",
                source,
                re.DOTALL,
            )
        )
        self.assertTrue(argv_of, "no AdmittedEffort constant parsed out of class.rs")
        # `const EFFORTS_ALL: &[AdmittedEffort] = &[LOW, MEDIUM, ..];`
        tiers_of = {
            name: [
                argv_of[member]
                for member in members.replace(" ", "").split(",")
                if member
            ]
            for name, members in re.findall(
                r"const (EFFORTS_\w+): &\[AdmittedEffort\] = &\[([^\]]*)\];", source
            )
        }
        self.assertTrue(tiers_of, "no EFFORTS_* constant parsed out of class.rs")

        table = rust_block(source, "\npub static MODEL_TABLE: &[ModelEntry] = &[\n")
        entries = re.findall(
            r"canonical: \"([^\"]+)\",\s*aliases: &\[([^\]]*)\],\s*efforts: (\w+),",
            table,
        )
        self.assertTrue(entries, "no ModelEntry parsed out of MODEL_TABLE")
        derived = {
            canonical: (
                [
                    alias.strip().strip('"')
                    for alias in aliases.split(",")
                    if alias.strip()
                ],
                tiers_of[efforts],
            )
            for canonical, aliases, efforts in entries
        }

        rows = table_with_header(self.readme(), "model", "aliases")
        documented = {}
        for row in rows:
            names = backticked(row[0])
            self.assertEqual(
                len(names), 1, f"README model row {row[0]!r} names {names}"
            )
            documented[names[0]] = (backticked(row[1]), backticked(row[2]))
        self.assertEqual(
            set(documented),
            set(derived),
            "README.md's model table is not MODEL_TABLE; a model the pool admits "
            "and the README omits is a model no caller knows to ask for",
        )
        for model, (aliases, tiers) in sorted(derived.items()):
            with self.subTest(model=model):
                self.assertEqual(documented[model][0], aliases)
                self.assertEqual(
                    documented[model][1],
                    tiers,
                    f"README.md offers `--effort` tiers for {model} that "
                    f"MODEL_TABLE does not admit, or omits ones it does",
                )

    # -- the MCP server --------------------------------------------------

    def test_the_readme_names_exactly_the_mcp_tools_the_server_exposes(self):
        """Asked of the server over stdio, not of a list in its source.

        One `initialize` and one `tools/list`, against a socket path that does
        not exist: `tools/list` is answered from the server's own definitions
        and never reaches a daemon, so this spends nothing and starts nothing.
        """

        request = (
            json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": {},
                        "clientInfo": {
                            "name": "gate-a-documented-surface",
                            "version": "0",
                        },
                    },
                }
            )
            + "\n"
            + json.dumps(
                {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}
            )
            + "\n"
        )
        socket = WORKSPACE / "target" / "no-such-socket-for-tools-list.sock"
        self.assertFalse(socket.exists(), "the tools/list probe must reach no daemon")
        replies = [
            json.loads(line)
            for line in run(
                [str(binary("pmux-mcp")), "--socket", str(socket)], stdin=request
            ).splitlines()
            if line.strip()
        ]
        listed = next(reply for reply in replies if reply.get("id") == 2)
        exposed = [tool["name"] for tool in listed["result"]["tools"]]
        self.assertTrue(exposed, "the MCP server listed no tool")

        readme = self.readme()
        _, _, mcp = readme.partition("\n### MCP\n")
        self.assertTrue(mcp, "README.md has no MCP section")
        sentence = next(
            (
                paragraph
                for paragraph in mcp.split("\n\n")
                if paragraph.startswith("It exposes exactly these tools:")
            ),
            None,
        )
        self.assertIsNotNone(
            sentence,
            "README.md's MCP section no longer opens its tool list with "
            "`It exposes exactly these tools:`, so the list is unchecked",
        )
        documented = backticked(sentence.split(".", 1)[0])
        self.assertEqual(
            sorted(documented),
            sorted(exposed),
            "README.md names a different set of MCP tools than the server answers "
            "`tools/list` with",
        )

    # -- the quickstart ---------------------------------------------------

    def test_the_readme_quickstart_starts_the_daemon_that_ask_needs(self):
        """The defect this test exists for, stated as a derivation.

        The quickstart used to start `pmuxd serve --socket .. --runtime-parent
        ..` and nothing else, which is a daemon on which EVERY `pmux ask` is
        refused -- so a reader who followed the README end to end could not
        reach the priority product at all.

        The required flags are read out of the refusal that reader would have
        hit. `path_b_not_enabled` is the one message on that path, and it names
        the flags that fix it; taking the requirement from there means the
        quickstart is checked against what a caller is actually told.
        """

        refusal = (
            WORKSPACE / "crates" / "service" / "src" / "pool" / "refusal.rs"
        ).read_text(encoding="utf-8")
        _, marker, body = refusal.partition(
            "pub fn path_b_not_enabled() -> ErrorBody {"
        )
        self.assertTrue(marker, "pool::refusal::path_b_not_enabled is gone")
        required = set(re.findall(r"--path-b-[a-z0-9-]+", body.split("\n}")[0]))
        self.assertTrue(required, "the Path B refusal names no flag to derive from")

        blocks = re.findall(r"```bash\n(.*?)```", self.readme(), re.DOTALL)
        serves = [block for block in blocks if "pmuxd serve" in block]
        self.assertTrue(serves, "README.md prints no `pmuxd serve` command")
        first = serves[0]
        for flag in sorted(required):
            with self.subTest(flag=flag):
                self.assertIn(
                    flag,
                    first,
                    f"the first `pmuxd serve` the README prints omits {flag}, so a "
                    f"reader who follows the quickstart gets a daemon that refuses "
                    f"every `pmux ask`",
                )


if __name__ == "__main__":
    unittest.main()
