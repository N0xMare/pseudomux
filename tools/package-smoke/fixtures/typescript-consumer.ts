import {
  PROTOCOL_VERSION,
  PmuxClaudeAgentTransport,
  PmuxClient,
  type StartSessionRequest,
  turnIdForAttempt,
} from "pmux-client";

const client: PmuxClient = new PmuxClient("/tmp/pmux-package-smoke.sock");
const transport: PmuxClaudeAgentTransport = new PmuxClaudeAgentTransport(client);
const protocolVersion: 1 = PROTOCOL_VERSION;
const turnId: string = turnIdForAttempt("package-artifact-type-smoke");
const start: StartSessionRequest = {
  identity: { mode: "new" },
  cwd: "/tmp",
  claude: { executable: "/usr/bin/false" },
  environment: { snapshot: {} },
  auth_policy: "subscription",
  terminal: { rows: 24, cols: 120, profile: "transparent", input_transport: "sdk" },
  lifecycle: { mode: "transcript" },
  retention: { mode: "persistent", idle_ttl_ms: 1_800_000 },
  compatibility: "require_tested",
};

void transport;
void protocolVersion;
void turnId;
void start;
