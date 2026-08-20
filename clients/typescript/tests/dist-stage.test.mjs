import assert from "node:assert/strict";
import { chmod, link, mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { fileURLToPath, pathToFileURL } from "node:url";

import {
  TYPESCRIPT_DIST_FILES,
  prepareTypescriptDistStage,
  verifyTypescriptDistStage,
} from "./dist-stage.mjs";

const WORKSPACE = fileURLToPath(new URL("../../../", import.meta.url)).replace(/\/$/, "");

async function withRoot(body) {
  const created = await mkdtemp(join(tmpdir(), "pmux-ts-dist-stage-"));
  const root = await realpath(created);
  await chmod(root, 0o700);
  try {
    await body(root);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
}

async function populate(root) {
  await prepareTypescriptDistStage(root, { outsideRoot: WORKSPACE });
  for (const name of TYPESCRIPT_DIST_FILES) {
    if (name === "package.json") continue;
    let bytes = "generated\n";
    if (name === "index.js") bytes = 'export const pmuxStageMarker = "external-esm-ok";\n';
    else if (name.endsWith(".js")) bytes = "export {};\n";
    else if (name.endsWith(".map")) bytes = "{}\n";
    else if (name.endsWith(".d.ts")) bytes = "export {};\n";
    await writeFile(join(root, name), bytes, { encoding: "utf8", flag: "wx", mode: 0o600 });
  }
}

test("stager requires an initially empty canonical private external root", async () => {
  await withRoot(async (root) => {
    await writeFile(join(root, "stale"), "stale", { mode: 0o600 });
    await assert.rejects(
      prepareTypescriptDistStage(root, { outsideRoot: WORKSPACE }),
      /requires an empty root/,
    );
  });
});

test("exact staged output is hashed and imports as ESM outside the package tree", async () => {
  await withRoot(async (root) => {
    await populate(root);
    const first = await verifyTypescriptDistStage(root, { outsideRoot: WORKSPACE });
    const second = await verifyTypescriptDistStage(root, { outsideRoot: WORKSPACE });
    assert.equal(first.manifest.length, TYPESCRIPT_DIST_FILES.length);
    assert.equal(first.sha256, second.sha256);
    assert.match(first.sha256, /^[0-9a-f]{64}$/);
    const api = await import(`${pathToFileURL(join(root, "index.js")).href}?stage-test=1`);
    assert.equal(api.pmuxStageMarker, "external-esm-ok");
  });
});

test("verifier rejects missing and extra output", async (context) => {
  await context.test("missing", async () => {
    await withRoot(async (root) => {
      await populate(root);
      await rm(join(root, "client.js.map"));
      await assert.rejects(verifyTypescriptDistStage(root), /exact membership mismatch/);
    });
  });
  await context.test("extra", async () => {
    await withRoot(async (root) => {
      await populate(root);
      await writeFile(join(root, "extra.js"), "export {};\n", { mode: 0o600 });
      await assert.rejects(verifyTypescriptDistStage(root), /exact membership mismatch/);
    });
  });
});

test("verifier rejects symlink, hardlink, directory, and public-mode substitutions", async (context) => {
  for (const [label, mutate, pattern] of [
    [
      "symlink",
      async (root) => {
        await rm(join(root, "client.js"));
        await symlink(join(root, "index.js"), join(root, "client.js"));
      },
      /regular file/,
    ],
    [
      "hardlink",
      async (root) => {
        await rm(join(root, "client.js"));
        await link(join(root, "index.js"), join(root, "client.js"));
      },
      /exactly one hard link/,
    ],
    [
      "directory",
      async (root) => {
        await rm(join(root, "client.js"));
        await mkdir(join(root, "client.js"), { mode: 0o700 });
      },
      /regular file/,
    ],
    [
      "public mode",
      async (root) => {
        await chmod(join(root, "client.js"), 0o644);
      },
      /mode must be 0600/,
    ],
  ]) {
    await context.test(label, async () => {
      await withRoot(async (root) => {
        await populate(root);
        await mutate(root);
        await assert.rejects(verifyTypescriptDistStage(root), pattern);
      });
    });
  }
});

test("verifier rejects changed ESM metadata and in-workspace roots", async (context) => {
  await context.test("metadata", async () => {
    await withRoot(async (root) => {
      await populate(root);
      await writeFile(join(root, "package.json"), '{"type":"commonjs"}\n', { mode: 0o600 });
      await assert.rejects(verifyTypescriptDistStage(root), /exact ESM scope/);
    });
  });
  await context.test("workspace containment", async () => {
    const inWorkspace = fileURLToPath(new URL("./", import.meta.url)).replace(/\/$/, "");
    await assert.rejects(
      verifyTypescriptDistStage(inWorkspace, { outsideRoot: WORKSPACE }),
      /outside the canonical workspace/,
    );
  });
});
