import type { components, paths } from './schema';
import { apiUrl } from './url';

export type ProblemDetails = components['schemas']['ProblemDetails'] & {
  instance?: string;
} & Record<string, unknown>;

export class ApiProblemError extends Error {
  constructor(readonly problem: ProblemDetails) {
    super(problem.detail || problem.title);
    this.name = 'ApiProblemError';
  }

  get status() {
    return this.problem.status;
  }
  get type() {
    return this.problem.type;
  }
  get title() {
    return this.problem.title;
  }
  get detail() {
    return this.problem.detail;
  }
  get instance() {
    return this.problem.instance;
  }
  get recovery() {
    return this.problem.recovery;
  }
  get errors() {
    return this.problem.errors;
  }
}

export class ApiHttpError extends Error {
  constructor(
    readonly status: number,
    guidance: string,
  ) {
    super(
      `GenAImage returned HTTP ${status} without usable problem details. ${guidance}`,
    );
    this.name = 'ApiHttpError';
  }
}

export class ApiNetworkError extends Error {
  constructor(guidance: string, cause: unknown) {
    super(`Could not complete the request to GenAImage. ${guidance}`, {
      cause,
    });
    this.name = 'ApiNetworkError';
  }
}

export class ApiResponseError extends Error {
  constructor(guidance: string, cause: unknown) {
    super(`Could not read the GenAImage response. ${guidance}`, { cause });
    this.name = 'ApiResponseError';
  }
}

function isProblem(value: unknown): value is ProblemDetails {
  if (!value || typeof value !== 'object') return false;
  const problem = value as Partial<ProblemDetails>;
  return (
    typeof problem.status === 'number' &&
    typeof problem.type === 'string' &&
    typeof problem.title === 'string' &&
    typeof problem.detail === 'string' &&
    typeof problem.recovery === 'string' &&
    (problem.instance === undefined || typeof problem.instance === 'string')
  );
}

function preserveAbort(error: unknown, signal?: AbortSignal | null): void {
  signal?.throwIfAborted();
  if (
    error &&
    typeof error === 'object' &&
    'name' in error &&
    error.name === 'AbortError'
  ) {
    throw error;
  }
}

type Path = keyof paths;
type Method = 'get' | 'put' | 'post';
type Operation<P extends Path, M extends Method> = NonNullable<paths[P][M]>;
type Params<P extends Path, M extends Method> = Pick<
  Operation<P, M>['parameters'],
  'path' | 'query' | 'header'
>;
type JsonBody<O> = O extends {
  requestBody: { content: { 'application/json': infer B } };
}
  ? B
  : never;
type MultipartBody<O> = O extends {
  requestBody: { content: { 'multipart/form-data': infer B } };
}
  ? { [K in keyof B]: File }
  : never;
type BodyOptions<O> = [JsonBody<O>] extends [never]
  ? [MultipartBody<O>] extends [never]
    ? { body?: never; files?: never }
    : { body?: never; files: MultipartBody<O> }
  : { body: JsonBody<O>; files?: never };
type SuccessJson<R> = {
  [K in keyof R]: K extends number
    ? `${K}` extends `2${string}`
      ? R[K] extends { content: { 'application/json': infer B } }
        ? B
        : never
      : never
    : never;
}[keyof R];
type JsonPath<M extends Method> = {
  [P in Path]: [SuccessJson<Operation<P, M>['responses']>] extends [never]
    ? never
    : P;
}[Path];

export interface RequestOptions {
  signal?: AbortSignal;
}
export type RunQuery = Params<'/api/v1/runs', 'get'>['query'];
export type RunEventsQuery = Params<'/api/v1/runs/{id}/events', 'get'>['query'];
export type OutputQuery = Params<'/api/v1/outputs', 'get'>['query'];
export type ThumbnailSize = Params<
  '/api/v1/outputs/{id}/thumbnail',
  'get'
>['query']['size'];
export type SubmitRunRequest = JsonBody<Operation<'/api/v1/runs', 'post'>>;
export type WorkflowValidationRequest = JsonBody<
  Operation<'/api/v1/workflows/{id}/validate', 'post'>
>;
export type SettingsPatch = JsonBody<Operation<'/api/v1/settings', 'put'>>;

export const outputMediaUrl = (id: string) =>
  apiUrl('/api/v1/outputs/{id}', { id });
export const outputThumbnailUrl = (id: string, size: ThumbnailSize) =>
  apiUrl('/api/v1/outputs/{id}/thumbnail', { id }, { size });
export const workflowThumbnailUrl = (id: string) =>
  apiUrl('/api/v1/workflows/{id}/thumbnail', { id });

// Read the original token before JSON number rounding can change a seed.
async function readExactIntegers(response: Response, bindings = false) {
  return parseBindingJson(await response.text(), bindings);
}

class ExactNumberReadError extends Error {
  constructor() {
    super(
      'Your browser cannot read numbers exactly; update to a current Chromium, Firefox or Safari release.',
    );
    this.name = 'ExactNumberReadError';
  }
}

function requireExactNumberReads() {
  let source: string | undefined;
  JSON.parse('2.0', (_key, value: unknown, context?: { source?: string }) => {
    source = context?.source;
    return value;
  });
  if (source !== '2.0') throw new ExactNumberReadError();
}

export function parseBindingJson(text: string, bindings = true): unknown {
  if (bindings) requireExactNumberReads();
  return JSON.parse(
    text,
    (_key, value: unknown, context?: { source?: string }) => {
      if (
        bindings &&
        typeof value === 'number' &&
        Number.isInteger(value) &&
        (Object.is(value, -0) || /[.eE]/.test(context?.source ?? ''))
      )
        return new BindingFloat(value);
      if (
        typeof value === 'number' &&
        Number.isInteger(value) &&
        !Number.isSafeInteger(value)
      ) {
        if (context?.source && /^-?\d+$/.test(context.source))
          return BigInt(context.source);
      }
      return value;
    },
  );
}

// A whole-valued float and an integer are distinct serde_json enum values.
export class BindingFloat {
  constructor(readonly value: number) {}
  toString(): string {
    const text = Object.is(this.value, -0) ? '-0' : String(this.value);
    return /[.eE]/.test(text) ? text : `${text}.0`;
  }
}

async function readWorkflowBindings(response: Response) {
  const details: components['schemas']['WorkflowDetails'] =
    (await readExactIntegers(
      response,
      true,
    )) as components['schemas']['WorkflowDetails'];
  for (const parameter of Object.values(details.parameters)) {
    for (const key of ['minimum', 'maximum', 'step'] as const) {
      const value: unknown = parameter[key];
      if (value instanceof BindingFloat) parameter[key] = value.value;
      else if (typeof value === 'bigint') parameter[key] = Number(value);
    }
  }
  return details;
}

export function supportsExactNumberWrites(): boolean {
  return typeof (JSON as JsonWithRaw).rawJSON === 'function';
}

type JsonWithRaw = JSON & { rawJSON?: (source: string) => unknown };
export function stringifyBindingJson(value: unknown): string {
  return JSON.stringify(value, writeBindingNumber);
}

function writeBindingNumber(_key: string, value: unknown): unknown {
  if (typeof value !== 'bigint' && !(value instanceof BindingFloat))
    return value;
  const raw = (JSON as JsonWithRaw).rawJSON;
  if (!raw) throw new Error('This browser cannot send exact JSON numbers.');
  return raw(value.toString());
}

/** Pages share `api`; inject fetch via this factory in tests. No request is retried. */
export function createApiClient(
  fetcher: typeof fetch = (input, init) => fetch(input, init),
) {
  async function request<T>(
    url: string,
    init: RequestInit,
    read: (response: Response) => Promise<T>,
  ): Promise<T> {
    const guidance =
      init.method === 'POST' &&
      (url === '/api/v1/runs' || /^\/api\/v1\/runs\/[^/]+\/rerun$/.test(url))
        ? 'The submission may have gone through. Check the run list and reconcile before submitting again.'
        : 'Check that the local GenAImage server is running and up to date.';
    let response: Response;
    try {
      init.signal?.throwIfAborted();
      response = await fetcher(url, { ...init, redirect: 'error' });
    } catch (error) {
      preserveAbort(error, init.signal);
      throw new ApiNetworkError(guidance, error);
    }
    if (!response.ok) {
      const contentType = response.headers
        .get('content-type')
        ?.split(';')[0]
        .trim()
        .toLowerCase();
      if (contentType === 'application/problem+json') {
        const problem: unknown = await response
          .json()
          .catch((error: unknown) => {
            preserveAbort(error, init.signal);
            return undefined;
          });
        if (isProblem(problem)) throw new ApiProblemError(problem);
      }
      throw new ApiHttpError(response.status, guidance);
    }
    try {
      return await read(response);
    } catch (error) {
      preserveAbort(error, init.signal);
      if (error instanceof ExactNumberReadError) throw error;
      throw new ApiResponseError(guidance, error);
    }
  }

  function json<M extends Method, P extends JsonPath<M>>(
    method: M,
    path: P,
    options: RequestOptions &
      Params<NoInfer<P>, NoInfer<M>> &
      BodyOptions<Operation<NoInfer<P>, NoInfer<M>>>,
    read = (response: Response) => response.json(),
  ): Promise<SuccessJson<Operation<P, M>['responses']>> {
    const headers = new Headers({
      Accept: 'application/json, application/problem+json',
    });
    for (const [name, value] of Object.entries(options.header ?? {})) {
      if (value != null) headers.set(name, String(value));
    }
    let body: BodyInit | undefined;
    if (options.files) {
      const form = new FormData();
      for (const [name, file] of Object.entries<File>(options.files))
        form.append(name, file);
      body = form;
    } else if (options.body !== undefined) {
      headers.set('Content-Type', 'application/json');
      body = stringifyBindingJson(options.body);
    }
    return request(
      apiUrl(path, options.path, options.query),
      {
        method: method.toUpperCase(),
        headers,
        body,
        signal: options.signal,
      },
      read,
    );
  }

  async function settingsSnapshot(response: Response) {
    const etag = response.headers.get('etag');
    if (!etag || etag.startsWith('W/'))
      throw new Error('The server did not provide a strong settings ETag.');
    const settings =
      (await response.json()) as components['schemas']['SettingsResponse'];
    return { settings, etag };
  }

  return {
    health: (options: RequestOptions = {}) =>
      json('get', '/api/v1/health', options),
    listWorkflows: (options: RequestOptions = {}) =>
      json('get', '/api/v1/workflows', options),
    getWorkflow: (
      id: string,
      options?: RequestOptions & { exactBindings?: boolean },
    ) =>
      json(
        'get',
        '/api/v1/workflows/{id}',
        { ...options, path: { id } },
        options?.exactBindings ? readWorkflowBindings : undefined,
      ),
    validateWorkflow: async (
      id: string,
      body: WorkflowValidationRequest,
      options?: RequestOptions & { exactNumbers?: boolean },
    ) => {
      if (options?.exactNumbers) requireExactNumberReads();
      return json('post', '/api/v1/workflows/{id}/validate', {
        ...options,
        path: { id },
        body,
      });
    },
    listRuns: (query?: RunQuery, options?: RequestOptions) =>
      json('get', '/api/v1/runs', { ...options, query }),
    getRun: (id: string, options?: RequestOptions) =>
      json('get', '/api/v1/runs/{id}', { ...options, path: { id } }),
    getRecordedRun: (
      id: string,
      options?: RequestOptions & { exactBindings?: boolean },
    ) =>
      json(
        'get',
        '/api/v1/runs/{id}',
        { ...options, path: { id } },
        (response) => readExactIntegers(response, options?.exactBindings),
      ),
    createRun: async (
      body: SubmitRunRequest,
      idempotencyKey: string,
      options?: RequestOptions,
    ) => {
      requireExactNumberReads();
      return json('post', '/api/v1/runs', {
        ...options,
        body,
        header: { 'Idempotency-Key': idempotencyKey },
      });
    },
    rerunRun: (id: string, idempotencyKey: string, options?: RequestOptions) =>
      json('post', '/api/v1/runs/{id}/rerun', {
        ...options,
        path: { id },
        header: { 'Idempotency-Key': idempotencyKey },
      }),
    cancelRun: (id: string, options?: RequestOptions) =>
      json('post', '/api/v1/runs/{id}/cancel', { ...options, path: { id } }),
    reconcileRun: (id: string, options?: RequestOptions) =>
      json('post', '/api/v1/runs/{id}/reconcile', { ...options, path: { id } }),
    reconcileRuns: (options: RequestOptions = {}) =>
      json('post', '/api/v1/runs/reconcile', options),
    getResolvedWorkflow: (id: string, options?: RequestOptions) =>
      json(
        'get',
        '/api/v1/runs/{id}/resolved-workflow',
        { ...options, path: { id } },
        readRecordedRun,
      ),
    getRunManifest: (id: string, options?: RequestOptions) =>
      json('get', '/api/v1/runs/{id}/manifest', { ...options, path: { id } }),
    getRunManifestText: (id: string, options?: RequestOptions) =>
      request(
        apiUrl('/api/v1/runs/{id}/manifest', { id }),
        options ?? {},
        async (response) => ({
          text: await response.text(),
          integrityVerified: !response.headers.has(
            'x-genaimage-manifest-integrity',
          ),
        }),
      ),
    listRunEvents: (
      id: string,
      query?: RunEventsQuery,
      options?: RequestOptions,
    ) =>
      json('get', '/api/v1/runs/{id}/events', {
        ...options,
        path: { id },
        query,
      }),
    listOutputs: (query?: OutputQuery, options?: RequestOptions) =>
      json('get', '/api/v1/outputs', { ...options, query }),
    getOutputMetadata: (id: string, options?: RequestOptions) =>
      json('get', '/api/v1/outputs/{id}/metadata', {
        ...options,
        path: { id },
      }),
    // Preserve the original bytes API for callers that need a Blob.
    getOutput: (id: string, options?: RequestOptions) =>
      request(outputMediaUrl(id), options ?? {}, (response) => response.blob()),
    upload: (file: File, options?: RequestOptions) =>
      json('post', '/api/v1/uploads', { ...options, files: { file } }),
    getUpload: (id: string, options?: RequestOptions) =>
      json('get', '/api/v1/uploads/{id}', { ...options, path: { id } }),
    getSettings: (options: RequestOptions = {}) =>
      json('get', '/api/v1/settings', options),
    getSettingsWithRevision: (options: RequestOptions = {}) =>
      request(
        apiUrl('/api/v1/settings'),
        {
          method: 'GET',
          headers: { Accept: 'application/json, application/problem+json' },
          signal: options.signal,
        },
        settingsSnapshot,
      ),
    putSettingsWithRevision: (
      body: SettingsPatch,
      etag: string,
      options: RequestOptions = {},
    ) =>
      request(
        apiUrl('/api/v1/settings'),
        {
          method: 'PUT',
          headers: {
            Accept: 'application/json, application/problem+json',
            'Content-Type': 'application/json',
            'If-Match': etag,
          },
          body: JSON.stringify(body),
          signal: options.signal,
        },
        settingsSnapshot,
      ),
    putSettings: (body: SettingsPatch, options?: RequestOptions) =>
      json('put', '/api/v1/settings', { ...options, body }),
    testSettings: (
      body: JsonBody<Operation<'/api/v1/settings/test', 'post'>>,
      options?: RequestOptions & {
        service?: 'comfyui' | 'llm';
        ifMatch?: string;
      },
    ) =>
      json('post', '/api/v1/settings/test', {
        signal: options?.signal,
        query: options?.service ? { service: options.service } : undefined,
        header: options?.ifMatch ? { 'If-Match': options.ifMatch } : undefined,
        body,
      }),
  };
}

export const api = createApiClient();
