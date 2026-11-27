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
      { key: 'llm.text_model', label: 'Text model' },
      { key: 'llm.vision_model', label: 'Vision model' },
      { key: 'llm.allowed_hosts', label: 'LLM allowed hosts', kind: 'hosts' },
      {
        key: 'llm.connect_timeout_secs',
        label: 'Connect timeout (seconds)',
        kind: 'number',
      },
      {
        key: 'llm.request_timeout_secs',
        label: 'Request timeout (seconds)',
        kind: 'number',
      },
      {
        key: 'llm.idle_timeout_secs',
        label: 'Idle timeout (seconds)',
        kind: 'number',
      },
    ],
  },
  {
    title: 'Paths',
    fields: [
      { key: 'paths.state_database', label: 'State database', kind: 'path' },
      { key: 'paths.workflows', label: 'Workflows directory', kind: 'path' },
      { key: 'paths.runs', label: 'Runs directory', kind: 'path' },
      { key: 'paths.uploads', label: 'Uploads directory', kind: 'path' },
      { key: 'paths.templates', label: 'Templates directory', kind: 'path' },
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
function ipAddress(value: string): string | undefined {
  if (
    /^(?:0|[1-9]\d{0,2})(?:\.(?:0|[1-9]\d{0,2})){3}$/.test(value) &&
    value.split('.').every((part) => Number(part) <= 255)
  )
    return value;
  if (
    !value.includes(':') ||
    value.includes('[') ||
    value.includes(']') ||
    /[\s%]/.test(value)
  )
    return;
  try {
    return new URL(`http://[${value}]/`).hostname.slice(1, -1);
  } catch {
    return;
  }
}
function isLoopback(host: string): boolean {
  if (host.toLowerCase() === 'localhost') return true;
  const ip = ipAddress(host);
  return (
    ip === '::1' ||
    (!!ip &&
      (ip.startsWith('127.') ||
        /^::ffff:7f[0-9a-f]{2}:[0-9a-f]{1,4}$/.test(ip)))
  );
}
function isLocalIp(ip: string): boolean {
  if (isLoopback(ip)) return true;
  if (ip.includes('.')) {
    const [a, b] = ip.split('.').map(Number);
    return (
      a === 10 ||
      (a === 172 && b >= 16 && b <= 31) ||
      (a === 192 && b === 168) ||
      (a === 169 && b === 254)
    );
  }
  const mapped = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(ip);
  if (mapped) {
    const a = parseInt(mapped[1], 16),
      b = parseInt(mapped[2], 16);
    return isLocalIp(`${a >> 8}.${a & 255}.${b >> 8}.${b & 255}`);
  }
  const first = parseInt(ip.split(':')[0] || '0', 16);
  return (first & 0xfe00) === 0xfc00 || (first & 0xffc0) === 0xfe80;
}
function parseHosts(value: string): string[] | undefined {
  try {
    const hosts: unknown = JSON.parse(value);
    if (
      !Array.isArray(hosts) ||
      hosts.some(
        (host) =>
          typeof host !== 'string' ||
          !host ||
          (/[/:* @]/.test(host) && !ipAddress(host)),
      )
    )
      return;
    return hosts;
  } catch {
    return;
  }
}
function validUrl(
  value: string,
  loopbackOnly: boolean,
  hosts: string[],
): string | undefined {
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
    const hostname = url.hostname.replace(/^\[|\]$/g, '');
    const ip = ipAddress(hostname);
    if (ip && !isLocalIp(ip))
      return 'Public and non-local addresses are not permitted.';
    if (loopbackOnly && !isLoopback(hostname))
      return 'Use a loopback URL here. Configure private or Compose hosts in the file or environment.';
    if (
      !isLoopback(hostname) &&
      !hosts.some((host) => host.toLowerCase() === hostname.toLowerCase())
    )
      return 'Add the endpoint host to the proposed allow-list.';
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
    if (!setting) continue;
    if (setting.read_only && !field.key.endsWith('.base_url')) continue;
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
      if (!parseHosts(value))
        errors[field.key] =
          'Enter a JSON array of exact hostnames or IP addresses, for example ["comfyui"].';
    } else if (field.key === 'server.host' && !isLoopback(value))
      errors[field.key] = 'Use a loopback host: localhost, 127.0.0.0/8 or ::1.';
    else if (
      (field.key === 'comfyui.base_url' || field.key === 'llm.base_url') &&
      (value !== '' || field.key === 'comfyui.base_url')
    ) {
      const hostKey = field.key.replace('base_url', 'allowed_hosts');
      const hosts = parseHosts(form[hostKey]) ?? [];
      const error = validUrl(value, value !== display(setting.value), hosts);
      if (error) errors[field.key] = error;
    }
  }
  return errors;
}
export function patchFrom(
  form: Form,
  settings: Settings,
  service?: string,
): Record<string, unknown> {
  const patch: Record<string, unknown> = {};
  for (const field of fields) {
    const setting = settings.settings[field.key];
    if (
      !setting ||
      (service !== undefined && !field.key.startsWith(`${service}.`)) ||
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
