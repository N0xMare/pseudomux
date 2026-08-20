/** Protocol DTOs are goldens, not product; living callers use PmuxClient / PmuxMessages. */
export * from "./client.js";
export * from "./messages.js";
export {
  MAX_NATIVE_FRAME_BYTES,
  MAX_SAFE_JSON_INTEGER,
  PMUX_ERROR_CODES,
  PROTOCOL_VERSION,
  V1_TAGGED_UNIONS,
  V1_VALUE_ENUMS,
  missingHealthLayers,
} from "./protocol.js";
export type {
  DaemonDiagnosis,
  EffortLevel,
  ErrorBody,
  HealthLayer,
  HealthLayerName,
  LayerFinding,
  PmuxErrorCode,
  RunStatelessRequest,
  StatelessResult,
  StopReason,
  TokenUsage,
  UsageBreakdown,
} from "./protocol.js";
