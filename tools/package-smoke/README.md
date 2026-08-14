# Package artifact smoke gates

These tracked gates prove that the TypeScript and Python client artifacts—not
only their source trees—are buildable, installable, and usable by isolated
consumers. They are packaging/evidence mechanics; protocol and service
semantics remain owned by the Rust and native-client conformance suites.

Both gates:

- require an externally hash-anchored, mode-`0600` candidate closure that names
  the exact runtime, interpreter scripts, dependency trees, and candidate
  support attestation used by that one gate;
- retain every declared input through no-follow file or directory descriptors
  and revalidate it before and after every child command;
- stage package inputs below one identity-fenced system temporary directory and
  clean it recursively through retained directory descriptors without following
  replacements or aliases;
- construct an explicit environment allowlist so caller credentials, proxies,
  user package configuration, and language injection variables are absent;
- disable package scripts, registry/index dependency acquisition, audits,
  funding requests, dependency resolution, and repository-local caches (this
  is not an OS sandbox and makes no claim that arbitrary socket access is
  blocked);
- build the actual npm tarball or Python wheel;
- stream-hash and structurally inspect the archive under per-entry, cumulative
  decompressed-byte, retained-metadata, and path/type limits without extracting
  untrusted paths or retaining ordinary entry payloads;
- require the exact public file closure in the archive, install that exact
  artifact into a private consumer root, and require the installed file and
  directory closure to match the artifact plus only explicitly modeled
  installer metadata;
- import the documented public API only from the installed artifact;
- validate runtime entrypoints, declarations or `py.typed`, and package
  metadata;
- run each finite command through the reviewed shared bounded-process
  implementation, recording its full structured receipt (bound executable,
  argv, cwd, redacted environment identity, standard-input identity, output
  hashes, process ledger, deadlines, and cleanup outcome), while enforcing an
  8 MiB combined output ceiling and terminating owned descendants, including
  inherited-pipe holders and double-fork/`setsid` escapes; and
- remove only the exact owned temporary root, then prove both client source
  trees are byte-for-byte unchanged.

The TypeScript gate requires the repository's locked `npm ci` dependencies to
already exist in `clients/typescript/node_modules`. The Python gate invokes the
declared setuptools PEP 517 backend directly inside the isolated interpreter,
then uses the declared pip only to install the already-created local wheel
with its index and dependency processing disabled. Neither gate downloads
tooling or dependencies, and no ambient backend subprocess is admitted.

## The Python interpreter contract

`clients/python/pyproject.toml` declares `build-backend =
"setuptools.build_meta"` with `requires = ["setuptools>=61"]`, and this gate
builds the wheel through exactly that backend with the index disabled. So the
declared `python_build_support_tree` must carry distribution metadata for every
name in `package_smoke.PYTHON_BUILD_SUPPORT_DISTRIBUTIONS` — today `pip` and
`setuptools` — and `validate_python_tool_report` refuses by name when it does
not, rather than reporting only that something "is not exact".

**Python 3.12 stopped shipping `setuptools` through `ensurepip`,** so a current
interpreter usually has pip and no setuptools. That is a fact about the host,
not about pmux: a user running `pip install clients/python` gets setuptools from
pip's build isolation as usual, and it matters here only because this gate
refuses to reach the network. The contract is therefore stated rather than
assumed, in three places that must agree:

- the tuple above, which the validator enforces;
- `validate_python_tool_report`'s named refusal, covered by
  `test_a_build_support_tree_missing_a_distribution_is_refused_by_name`, which
  runs on any interpreter because it validates a synthetic report; and
- `test_real_python_package_flow_with_materialized_fixture_closure`, whose
  fixture materializes the support tree *out of the running interpreter*. It
  now checks the same tuple first and **skips, naming the missing distribution
  and the interpreter**, instead of failing three frames deep inside
  `importlib.metadata` — which is what a `gate_f/package_smoke_self_tests` cell
  did on a stock Python 3.13 host, reporting a host property as a product
  defect. Where that flow is wanted end to end, run the cell under an
  interpreter with setuptools installed; the skip says so.

The Gate A candidate runner creates the candidate closure and exports these
five anchors before each invocation:

- `PMUX_PACKAGE_SMOKE_CLOSURE_FILE`
- `PMUX_PACKAGE_SMOKE_CLOSURE_SHA256`
- `PMUX_PACKAGE_SMOKE_CANDIDATE_SHA256`
- `PMUX_PACKAGE_SMOKE_SOURCE_SHA256`
- `PMUX_PACKAGE_SMOKE_PREVIOUS_ANCHOR_SHA256`

The closure has an exact per-language role inventory. TypeScript binds
`node_executable`, `npm_executable`, `npm_support_tree`,
`typescript_compiler`, `typescript_dependency_tree`,
`node_types_dependency_tree`, `undici_types_dependency_tree`, and
`support_closure`. The npm CLI must be a member of the materialized npm support
tree; the compiler must be a member of the locked TypeScript tree, and the
separate locked `undici-types` role closes `@types/node`'s package-lock
dependency. Python binds `python_executable`,
`python_stdlib_tree`, `python_dynload_tree`,
`python_build_support_tree`, and `support_closure`. The Python support tree is
the exact isolated pip/setuptools import and distribution closure used for
building and installing the wheel; unrelated site packages are not admitted,
and absent standalone `build`, `wheel`, and Ruff distributions are reported as
absent rather than inferred. Setuptools-added vendor paths and their complete
distribution inventory—including its bound vendored wheel implementation—are
reported separately and must remain inside that same materialized tree. Role
trees are pairwise non-overlapping, and no file may be hidden inside a role
tree except the two intentional TypeScript
interpreter-script relationships. Every input carries both a portable content
digest and a host witness digest. An invocation without this candidate-supplied
materialized closure fails closed.

The candidate is the sole authority that selects and materializes those paths,
writes the private manifests, and advances the external anchor chain. It calls
the side-effect-free `declared_input_record`,
`candidate_support_closure_payload`, and `declared_closure_payload` helpers in
this module so construction and verification share the exact domain-separated
digest formats without duplicating them. The helpers only inspect explicitly
selected paths and return canonical-JSON-serializable dictionaries; they do not
choose tools, write files, change the environment, or run commands.

Every Python command bootstrap runs with `-I -S -B`, replaces `sys.path`
with only the declared stdlib, extension, and build-support roots, and verifies
the resulting executable, flags, module origins, distribution inventory, and
exact path order. `-B` is explicit because isolated mode ignores the
`PYTHONDONTWRITEBYTECODE` environment variable; this keeps all declared support
trees immutable across the gate.

Run the two gates independently under that candidate environment so the
release manifest records a separate result and artifact identity for each
package:

```bash
python3 tools/package-smoke/package_smoke.py typescript
python3 tools/package-smoke/package_smoke.py python
```

Focused acquisition/archive/cleanup self-tests are:

```bash
PYTHONDONTWRITEBYTECODE=1 \
  python3 -m unittest discover -s tools/package-smoke/tests -v
```

The focused suite includes a real wheel build/install/import using a freshly
materialized Python closure. Its real npm-flow cell runs when
`PMUX_PACKAGE_SMOKE_TEST_NODE` names the direct Node executable and
`PMUX_PACKAGE_SMOKE_TEST_NPM_TREE` names the complete npm support-tree source;
the Gate A candidate runner supplies those values when it constructs the
frozen candidate closure.

Each successful gate writes one canonical JSON report to stdout. The report
contains the exact package version, artifact filename/size/SHA-256, archive
content-manifest digest, relevant npm integrity or wheel metadata, installed
consumer observations, tool versions, candidate/dependency-closure anchors,
the exact shared bounded-process implementation SHA-256, and every command
receipt. Portable installed-closure digests normalize ephemeral locations;
the command receipts deliberately retain their exact host-local cwd and argv
paths so the execution evidence remains falsifiable. The artifact itself is
not retained outside the release evidence system.
