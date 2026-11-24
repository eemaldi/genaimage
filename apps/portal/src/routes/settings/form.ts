import type { components } from '../../lib/api/schema';

export type Settings = components['schemas']['SettingsResponse'];
export type Field = {
  key: string;
  label: string;
  kind?: 'number' | 'hosts' | 'path';
};
export const sections: { title: string; fields: Field[] }[] = [
  {
    title: 'ComfyUI',
    fields: [
      { key: 'comfyui.base_url', label: 'ComfyUI URL' },
      {
        key: 'comfyui.allowed_hosts',
        label: 'ComfyUI allowed hosts',
        kind: 'hosts',
      },
      {
        key: 'comfyui.request_timeout_seconds',
        label: 'Request timeout (seconds)',
        kind: 'number',
      },
      {
        key: 'comfyui.websocket_reconnect_seconds',
        label: 'WebSocket reconnect delay (seconds)',
        kind: 'number',
      },
    ],
  },
  {
    title: 'LLM',
    fields: [
      { key: 'llm.base_url', label: 'LLM URL' },
      { key: 'llm.model', label: 'LLM model' },
      { key: 'llm.allowed_hosts', label: 'LLM allowed hosts', kind: 'hosts' },
    ],
  },
  {
    title: 'Paths',
    fields: [
      { key: 'paths.state_database', label: 'State database', kind: 'path' },
      { key: 'paths.workflows', label: 'Workflows directory', kind: 'path' },
      { key: 'paths.runs', label: 'Runs directory', kind: 'path' },
      { key: 'paths.uploads', label: 'Uploads directory', kind: 'path' },
    ],
  },
  {
    title: 'Server defaults',
    fields: [
      { key: 'server.host', label: 'Server host' },
      { key: 'server.port', label: 'Server port', kind: 'number' },
      {
        key: 'server.max_upload_bytes',
        label: 'Maximum upload size (bytes)',
        kind: 'number',
      },
      { key: 'claim_legacy_state', label: 'Claim legacy state' },
    ],
  },
];
export const fields = sections.flatMap((section) => section.fields);
export type Form = Record<string, string>;

export function display(value: unknown): string {
  return Array.isArray(value) ? JSON.stringify(value) : String(value ?? '');
}
export function formFrom(settings: Settings): Form {
  return Object.fromEntries(
    fields
      .filter(({ key }) => settings.settings[key])
      .map(({ key }) => [key, display(settings.settings[key].value)]),
  );
}
function isLoopback(host: string): boolean {
  const name = host.toLowerCase().replace(/^\[|\]$/g, '');
  return (
    name === 'localhost' ||
    name === '::1' ||
    (/^127\.(\d{1,3}\.){2}\d{1,3}$/.test(name) &&
      name.split('.').every((part) => Number(part) <= 255)) ||
    /^::ffff:7f[0-9a-f]{2}:[0-9a-f]{1,4}$/.test(name)
  );
}
function validUrl(value: string, loopbackOnly: boolean): string | undefined {
  try {
    const url = new URL(value);
    if (
      !/^https?:$/.test(url.protocol) ||
      url.username ||
      url.password ||
      value.includes('?') ||
      value.includes('#')
    )
      return 'Use an HTTP(S) URL without credentials, a query or a fragment.';
    if (loopbackOnly && !isLoopback(url.hostname))
      return 'Use a loopback URL here. Configure private or Compose hosts in the file or environment.';
  } catch {
    return 'Enter a valid HTTP(S) URL.';
  }
}
export function validate(
  form: Form,
  settings: Settings,
): Record<string, string> {
  const errors: Record<string, string> = {};
  for (const field of fields) {
    const setting = settings.settings[field.key];
    if (!setting || setting.read_only) continue;
    const value = form[field.key];
    if (field.kind === 'number') {
      const n = Number(value);
      if (
        !/^[0-9]+$/.test(value) ||
        !Number.isSafeInteger(n) ||
        n < 1 ||
        (field.key === 'server.port' && n > 65535)
      )
        errors[field.key] =
          field.key === 'server.port'
            ? 'Enter a port from 1 to 65535.'
            : 'Enter a positive whole number in range.';
    } else if (field.kind === 'path' && value.length === 0)
      errors[field.key] = 'Enter a non-empty path.';
    else if (
      field.key === 'paths.state_database' &&
      /(?:^|\/)\.{1,2}$|\/$/.test(value)
    )
      errors[field.key] = 'Choose a database file path.';
    else if (field.kind === 'hosts') {
      try {
        const hosts: unknown = JSON.parse(value);
        if (
          !Array.isArray(hosts) ||
          hosts.some(
            (host: unknown) =>
              typeof host !== 'string' ||
              !host ||
              /[/* @]/.test(host) ||
              (host.includes(':') && !/^[0-9a-f:]+$/i.test(host)),
          )
        )
          throw new Error();
      } catch {
        errors[field.key] =
          'Enter a JSON array of exact hostnames or IP addresses, for example ["comfyui"].';
      }
    } else if (field.key === 'server.host' && !isLoopback(value))
      errors[field.key] = 'Use a loopback host: localhost, 127.0.0.0/8 or ::1.';
    else if (
      (field.key === 'comfyui.base_url' || field.key === 'llm.base_url') &&
      (value !== '' || field.key === 'comfyui.base_url')
    ) {
      const error = validUrl(value, value !== display(setting.value));
      if (error) errors[field.key] = error;
    }
  }
  return errors;
}
export function patchFrom(
  form: Form,
  settings: Settings,
): Record<string, unknown> {
  const patch: Record<string, unknown> = {};
  for (const field of fields) {
    const setting = settings.settings[field.key];
    if (
      !setting ||
      setting.read_only ||
      form[field.key] === display(setting.value)
    )
      continue;
    patch[field.key] =
      field.kind === 'number'
        ? Number(form[field.key])
        : field.kind === 'hosts'
          ? JSON.parse(form[field.key])
          : form[field.key];
  }
  return patch;
}
