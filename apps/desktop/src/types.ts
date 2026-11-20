export type JsonValue = string | number | boolean | null | JsonValue[] | { [key: string]: JsonValue };
export type GateId =
  | "gate1"
  | "gate2"
  | "gate3"
  | "phase04_review"
  | "gate4"
  | "gate5"
  | "phase07_review"
  | "phase08_review";
export type ApprovalStatus = "pending" | "approved" | "rejected";
export type ParameterKind =
  | "string"
  | "integer"
  | "number"
  | "boolean"
  | "seed"
  | "image_upload";
export type RunStatus =
  | "created"
  | "submitting"
  | "unknown_submission"
  | "queued"
  | "running"
  | "completed"
  | "failed"
  | "cancelling"
  | "cancelled";

export interface ParameterDefinition {
  node: string;
  input: string;
  kind: ParameterKind;
  required: boolean;
  description: string;
  default: JsonValue;
  allowed_values: JsonValue[] | null;
  minimum: number | null;
  maximum: number | null;
}

export interface WorkflowManifest {
  schema_version: number;
  id: string;
  version: string;
  title: string;
  workflow: string;
  generation_mode: string;
  parameters: Record<string, ParameterDefinition>;
  presets: Record<string, Record<string, JsonValue>>;
  required_gates: GateId[];
  outputs: Array<{ node: string; kind: string }>;
}

export interface WorkflowInspection {
  manifest: WorkflowManifest;
  parameters: Array<ParameterSummary>;
  required_gates: GateId[];
  workflow_sha256: string;
}

export interface ParameterSummary {
  name: string;
  kind: ParameterKind;
  required: boolean;
  description: string;
  default: JsonValue;
  allowed_values: JsonValue[] | null;
}

export interface ResolvedConfig {
  workspace: string;
  active_profile: string;
  comfyui: {
    base_url: string;
    request_timeout_seconds: number;
    websocket_reconnect_seconds: number;
  };
  state_database: string;
  workflows: string;
  runs: string;
  characters: string;
}

export interface RunRecord {
  id: string;
  prompt_id: string;
  client_id: string;
  workflow_id: string;
  workflow_version: string;
  workflow_sha256: string;
  generation_mode: string;
  character_id: string | null;
  status: RunStatus;
  parameters: JsonValue;
  gate_snapshot: JsonValue | null;
  output_dir: string;
  manifest_path: string | null;
  error: string | null;
  created_at: string;
  updated_at: string;
  completed_at: string | null;
}

export interface OutputRecord {
  run_id: string;
  node_id: string;
  kind: string;
  source_filename: string;
  source_subfolder: string;
  source_type: string;
  local_path: string;
  sha256: string;
  byte_size: number;
  created_at: string;
}

export interface RunEvent {
  id: number;
  run_id: string;
  event_type: string;
  message: string | null;
  payload: JsonValue;
  created_at: string;
}

export interface RunDetails {
  run: RunRecord;
  events: RunEvent[];
  outputs: OutputRecord[];
}

export interface GateRecord {
  status: ApprovalStatus;
  decision: string | null;
  approver: string | null;
  decided_at: string | null;
}

export interface GateDecision {
  gate: GateId;
  status: ApprovalStatus;
  approver: string;
  decision: string;
  decided_at: string;
}

export interface CharacterState {
  schema_version: number;
  character_id: string;
  character_name: string | null;
  current_version: string;
  gates: Record<GateId, GateRecord>;
  decisions: GateDecision[];
}

export interface RunProgress {
  run: RunRecord;
  event: {
    event_type: string;
    prompt_id: string | null;
    node_id: string | null;
    value: number | null;
    maximum: number | null;
    message: string | null;
    raw: JsonValue;
  } | null;
}

