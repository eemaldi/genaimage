import { describe, expect, it } from "vitest";
import type { ParameterDefinition } from "./types";
import { formatBytes, isTrackable, parseParameterValue, statusLabel } from "./utils";

const definition = (kind: ParameterDefinition["kind"]): ParameterDefinition => ({
  node: "1",
  input: "value",
  kind,
  required: true,
  description: "",
  default: null,
  allowed_values: null,
  minimum: null,
  maximum: null
});

describe("desktop form and status helpers", () => {
  it("parses numeric values without string coercion", () => {
    expect(parseParameterValue(definition("integer"), "20")).toBe(20);
    expect(() => parseParameterValue(definition("integer"), "2.5")).toThrow(
      "whole number"
    );
    expect(parseParameterValue(definition("boolean"), "true")).toBe(true);
  });

  it("labels only non-terminal runs as trackable", () => {
    expect(isTrackable("running")).toBe(true);
    expect(isTrackable("unknown_submission")).toBe(false);
    expect(statusLabel("unknown_submission")).toBe("unknown submission");
  });

  it("formats byte counts", () => {
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(2048)).toBe("2.0 KiB");
  });
});
