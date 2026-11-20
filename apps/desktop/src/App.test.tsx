import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "./App";

const workflow = {
  schema_version: 1,
  id: "text-to-image",
  version: "1.0.0",
  title: "Explicit text-to-image",
  workflow: "api-workflow.json",
  generation_mode: "generic",
  parameters: {
    positive_prompt: {
      node: "6",
      input: "text",
      kind: "string",
      required: true,
      description: "Approved prompt",
      default: null,
      allowed_values: null,
      minimum: null,
      maximum: null
    }
  },
  presets: {},
  required_gates: [],
  outputs: [{ node: "9", kind: "image" }]
};

const invoke = vi.hoisted(() => vi.fn());

async function mockedInvoke(command: string) {
  switch (command) {
    case "current_workspace":
      return "/project";
    case "configuration":
      return {
        workspace: "/project",
        active_profile: "local",
        comfyui: {
          base_url: "http://127.0.0.1:8188",
          request_timeout_seconds: 60,
          websocket_reconnect_seconds: 2
        },
        state_database: "/project/.genaimage/state.sqlite3",
        workflows: "/project/workflows/comfyui",
        runs: "/project/runs",
        characters: "/project/characters"
      };
    case "list_workflows":
      return [workflow];
    case "list_runs":
      return [];
    case "health":
      return { system: {} };
    case "inspect_workflow":
      return {
        manifest: workflow,
        parameters: [],
        required_gates: [],
        workflow_sha256: "abc123"
      };
    default:
      throw new Error(`Unexpected command ${command}`);
  }
}

vi.mock("@tauri-apps/api/core", () => ({ invoke }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => () => undefined)
}));

describe("GenAImage desktop", () => {
  beforeEach(() => {
    invoke.mockClear();
    invoke.mockImplementation(mockedInvoke);
  });

  it("loads local harness status and workflow inventory", async () => {
    render(<App />);

    expect(await screen.findByRole("heading", { name: "Production overview" })).toBeVisible();
    await waitFor(() => expect(screen.getByText("ComfyUI connected")).toBeVisible());
    fireEvent.click(screen.getAllByRole("button", { name: "Launch workflow" })[0]);
    expect(
      await screen.findByRole("option", { name: /Explicit text-to-image/ })
    ).toBeInTheDocument();
  });
});
