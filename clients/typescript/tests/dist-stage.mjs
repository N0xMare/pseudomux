import { createHash } from "node:crypto";
import { constants } from "node:fs";
import { lstat, open, readdir, realpath } from "node:fs/promises";
import { isAbsolute, join, relative, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const PACKAGE_BYTES = Buffer.from('{"type":"module"}\n', "utf8");
const PRIVATE_DIRECTORY_MODE = 0o700;
const PRIVATE_FILE_MODE = 0o600;

export const TYPESCRIPT_DIST_FILES = Object.freeze([
  "client.d.ts",
  "client.d.ts.map",
  "client.js",
  "client.js.map",
  "index.d.ts",
  "index.d.ts.map",
  "index.js",
  "index.js.map",
  "package.json",
  "protocol.d.ts",
  "protocol.d.ts.map",
  "protocol.js",
  "protocol.js.map",
  "smithers.d.ts",
  "smithers.d.ts.map",
  "smithers.js",
  "smithers.js.map",
]);

function check(condition, message) {
  if (!condition) throw new Error(`TypeScript validation stage: ${message}`);
}

function isWithin(root, candidate) {
  const suffix = relative(root, candidate);
  return suffix === "" || (!suffix.startsWith("..") && !isAbsolute(suffix));
}

async function canonicalPrivateRoot(supplied, outsideRoot) {
  check(typeof supplied === "string" && isAbsolute(supplied), "root must be absolute");
  check(resolve(supplied) === supplied, "root must be normalized");
  const linkMetadata = await lstat(supplied);
  check(!linkMetadata.isSymbolicLink() && linkMetadata.isDirectory(), "root must be a directory");
  const canonical = await realpath(supplied);
  check(canonical === supplied, "root must be canonical");
  if (outsideRoot !== undefined) {
    check(typeof outsideRoot === "string" && isAbsolute(outsideRoot), "outside root must be absolute");
    const canonicalOutsideRoot = await realpath(outsideRoot);
    check(!isWithin(canonicalOutsideRoot, canonical), "root must be outside the canonical workspace");
  }
  check((linkMetadata.mode & 0o777) === PRIVATE_DIRECTORY_MODE, "root mode must be 0700");
  return canonical;
}

async function readStablePrivateFile(root, name) {
  const path = join(root, name);
  const linkMetadata = await lstat(path);
  check(!linkMetadata.isSymbolicLink() && linkMetadata.isFile(), `${name} must be a regular file`);
  check(linkMetadata.nlink === 1, `${name} must have exactly one hard link`);
  check((linkMetadata.mode & 0o777) === PRIVATE_FILE_MODE, `${name} mode must be 0600`);
  check((await realpath(path)) === path, `${name} must not escape the stage`);

  const descriptor = await open(path, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const before = await descriptor.stat();
    check(before.isFile() && before.nlink === 1, `${name} descriptor identity is invalid`);
    const bytes = await descriptor.readFile();
    const after = await descriptor.stat();
    for (const field of ["dev", "ino", "mode", "nlink", "size", "mtimeMs", "ctimeMs"]) {
      check(before[field] === after[field], `${name} changed while it was hashed`);
    }
    check(bytes.length === before.size, `${name} length changed while it was hashed`);
    return {
      bytes,
      device: before.dev,
      inode: before.ino,
      sha256: createHash("sha256").update(bytes).digest("hex"),
    };
  } finally {
    await descriptor.close();
  }
}

export async function prepareTypescriptDistStage(supplied, options = {}) {
  const root = await canonicalPrivateRoot(supplied, options.outsideRoot);
  const entries = await readdir(root);
  check(entries.length === 0, "prepare requires an empty root");
  const packagePath = join(root, "package.json");
  const descriptor = await open(
    packagePath,
    constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL | (constants.O_NOFOLLOW ?? 0),
    PRIVATE_FILE_MODE,
  );
  try {
    await descriptor.writeFile(PACKAGE_BYTES);
    await descriptor.sync();
  } finally {
    await descriptor.close();
  }
  const directory = await open(root, constants.O_RDONLY | (constants.O_DIRECTORY ?? 0));
  try {
    await directory.sync();
  } finally {
    await directory.close();
  }
  return root;
}

export async function verifyTypescriptDistStage(supplied, options = {}) {
  const root = await canonicalPrivateRoot(supplied, options.outsideRoot);
  const entries = (await readdir(root)).sort();
  check(
    JSON.stringify(entries) === JSON.stringify(TYPESCRIPT_DIST_FILES),
    `exact membership mismatch: ${JSON.stringify(entries)}`,
  );

  const identities = new Set();
  const manifest = [];
  for (const name of TYPESCRIPT_DIST_FILES) {
    const identity = await readStablePrivateFile(root, name);
    const key = `${identity.device}:${identity.inode}`;
    check(!identities.has(key), `${name} aliases another staged file`);
    identities.add(key);
    manifest.push({ relative_path: name, sha256: identity.sha256 });
    if (name === "package.json") {
      check(identity.bytes.equals(PACKAGE_BYTES), "package.json is not the exact ESM scope");
    }
  }
  const encoded = JSON.stringify({ schema_version: 1, files: manifest });
  return {
    root,
    manifest,
    sha256: createHash("sha256")
      .update("pmux-typescript-dist-stage-v1\0", "utf8")
      .update(encoded, "utf8")
      .digest("hex"),
  };
}

export async function clientModuleUrl(importMetaUrl) {
  const configured = process.env.PMUX_TYPESCRIPT_DIST_DIR;
  if (configured === undefined) return new URL("../dist/index.js", importMetaUrl);
  const stage = await verifyTypescriptDistStage(configured);
  return pathToFileURL(join(stage.root, "index.js"));
}

async function cli() {
  const arguments_ = process.argv.slice(2);
  check(arguments_.length === 2 || arguments_.length === 4, "expected two or four arguments");
  const [operation, root, option, outsideRoot] = arguments_;
  check(operation === "prepare" || operation === "verify", "operation must be prepare or verify");
  check(root !== undefined, "root argument is required");
  check(option === undefined || option === "--outside-root", "unknown option");
  check((option === undefined) === (outsideRoot === undefined), "outside-root option is incomplete");
  const options = outsideRoot === undefined ? {} : { outsideRoot };
  if (operation === "prepare") {
    await prepareTypescriptDistStage(root, options);
  } else {
    const stage = await verifyTypescriptDistStage(root, options);
    process.stdout.write(`${stage.sha256}\n`);
  }
}

const invokedPath = process.argv[1] === undefined ? undefined : await realpath(process.argv[1]);
if (invokedPath === (await realpath(fileURLToPath(import.meta.url)))) {
  await cli();
}
