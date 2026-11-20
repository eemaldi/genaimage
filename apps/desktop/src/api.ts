import { invoke } from "@tauri-apps/api/core";
import type {
  CharacterState,
  GateId,
  JsonValue,
  ResolvedConfig,
  RunDetails,
  RunRecord,
  WorkflowInspection,
  WorkflowManifest
} from "./types";

export interface SubmitRunRequest {
  workflow: string;
  preset: string | null;
  parameters: Record<string, JsonValue>;
  character_id: string | null;
  version: string | null;
}

export const api = {
  currentWorkspace: () => invoke<string>("current_workspace"),
  setWorkspace: (workspace: string) => invoke<string>("set_workspace", { workspace }),
  configuration: () => invoke<ResolvedConfig>("configuration"),
  setComfyUiUrl: (profile: string, baseUrl: string) =>
    invoke<ResolvedConfig>("set_comfyui_url", { profile, baseUrl }),
  health: () => invoke<unknown>("health"),
  listWorkflows: () => invoke<WorkflowManifest[]>("list_workflows"),
  inspectWorkflow: (workflow: string) =>
    invoke<WorkflowInspection>("inspect_workflow", { workflow }),
  submitRun: (request: SubmitRunRequest) => invoke<RunRecord>("submit_run", { request }),
  listRuns: (limit = 100) => invoke<RunRecord[]>("list_runs", { limit }),
  runDetails: (runId: string) => invoke<RunDetails>("run_details", { runId }),
  reconcileRuns: (runId: string | null = null) =>
    invoke<unknown>("reconcile_runs", { runId }),
  cancelRun: (runId: string) => invoke<RunRecord>("cancel_run", { runId }),
  characterStatus: (characterId: string) =>
    invoke<CharacterState>("character_status", { characterId }),
  initialiseCharacter: (characterId: string, name: string | null) =>
    invoke<CharacterState>("initialise_character", { characterId, name }),
  decideGate: (
    characterId: string,
    gate: GateId,
    approver: string,
    note: string,
    approved: boolean
  ) =>
    invoke<CharacterState>("decide_gate", {
      request: { character_id: characterId, gate, approver, note, approved }
    })
};
