import type { JsonValue, ParameterDefinition, RunStatus } from "./types";

export function statusLabel(status: RunStatus): string {
  return status.replaceAll("_", " ");
}

export function isTrackable(status: RunStatus): boolean {
  return ["created", "submitting", "queued", "running", "cancelling"].includes(status);
}

export function formValue(value: JsonValue): string {
  if (value === null) {
    return "";
  }
  return typeof value === "object" ? JSON.stringify(value) : String(value);
}

export function parseParameterValue(
  definition: ParameterDefinition,
  raw: string
): JsonValue {
  if (definition.kind === "integer" || definition.kind === "seed") {
    const value = Number(raw);
    if (!Number.isInteger(value)) {
      throw new Error("Enter a whole number.");
    }
    return value;
  }
  if (definition.kind === "number") {
    const value = Number(raw);
    if (!Number.isFinite(value)) {
      throw new Error("Enter a finite number.");
    }
    return value;
  }
  if (definition.kind === "boolean") {
    return raw === "true";
  }
  return raw;
}

export function shortHash(hash: string): string {
  return hash.length > 12 ? `${hash.slice(0, 12)}…` : hash;
}

export function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const units = ["KiB", "MiB", "GiB"];
  let value = bytes / 1024;
  let unit = units[0];
  for (let index = 1; index < units.length && value >= 1024; index += 1) {
    value /= 1024;
    unit = units[index];
  }
  return `${value.toFixed(1)} ${unit}`;
}
