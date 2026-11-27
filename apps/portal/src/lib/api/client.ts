import type { components, paths } from './schema';
import { apiUrl } from './url';
import type { SseFrame } from './sse';

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
export type SubmitBatchRequest = JsonBody<Operation<'/api/v1/batches', 'post'>>;
export type BatchQuery = Params<'/api/v1/batches', 'get'>['query'];
export type WorkflowValidationRequest = JsonBody<
  Operation<'/api/v1/workflows/{id}/validate', 'post'>
>;
export type WorkflowImportRequest = JsonBody<
  Operation<'/api/v1/workflows/import', 'post'>
>;
export type WorkflowImportConfirmRequest = JsonBody<
  Operation<'/api/v1/workflows/import/confirm', 'post'>
>;
export type SettingsPatch = JsonBody<Operation<'/api/v1/settings', 'put'>>;
export type PromptAssistRequest = JsonBody<
  Operation<'/api/v1/assistant/prompt', 'post'>
>;
export type TemplateRenderRequest = JsonBody<
  Operation<'/api/v1/templates/render', 'post'>
>;
export type SuggestWorkflowRequest = JsonBody<
  Operation<'/api/v1/assistant/suggest', 'post'>
>;
export type AssistantConversationQuery = Params<
  '/api/v1/assistant/conversations',
  'get'
>['query'];
export type SendAssistantMessageRequest = JsonBody<
  Operation<'/api/v1/assistant/conversations/{id}/messages', 'post'>
>;
export type AssistantStreamEvent =
  | ({
      event: 'message_started';
    } & components['schemas']['AssistantMessageStartedEvent'])
  | ({ event: 'delta' } & components['schemas']['AssistantDeltaEvent'])
  | ({ event: 'tool_call' } & components['schemas']['AssistantToolCallEvent'])
  | ({
      event: 'tool_result';
    } & components['schemas']['AssistantToolResultEvent'])
  | ({
      event: 'confirmation_required';
    } & components['schemas']['AssistantConfirmationRequiredEvent'])
  | ({ event: 'done' } & components['schemas']['AssistantTerminalEvent'])
  | ({
      event: 'awaiting_confirmation';
    } & components['schemas']['AssistantAwaitingConfirmationEvent'])
  | ({ event: 'error' } & components['schemas']['AssistantTerminalEvent']);
type WithoutEvent<T> = T extends { event: unknown } ? Omit<T, 'event'> : never;
type AssistantStreamPayload = WithoutEvent<AssistantStreamEvent>;
export type AssistantStream = AsyncGenerator<SseFrame<AssistantStreamPayload>>;

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

export function requireExactNumberReads() {
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
  constructor(
    readonly value: number,
    private readonly source?: string,
  ) {}
  toString(): string {
    if (this.source !== undefined) return this.source;
    const text = Object.is(this.value, -0) ? '-0' : String(this.value);
    return /[.eE]/.test(text) ? text : `${text}.0`;
  }
}

/** Parse assistant JSON while retaining every numeric token's wire spelling. */
export function parseAssistantJson(text: string): unknown {
  requireExactNumberReads();
  return JSON.parse(
    text,
    (_key, value: unknown, context?: { source?: string }) => {
      const source = context?.source;
      if (typeof value !== 'number' || !source) return value;
      if (/^-?\d+$/.test(source)) {
        if (source === '-0') return new BindingFloat(value, source);
        if (!Number.isSafeInteger(value)) return BigInt(source);
        return value;
      }
      return new BindingFloat(value, source);
    },
  );
}

export function formatExactJson(value: unknown, space = 2): string {
  function format(current: unknown, depth: number): string {
    if (current instanceof BindingFloat) return current.toString();
    if (typeof current === 'bigint') return current.toString();
    if (
      current === null ||
      typeof current === 'string' ||
      typeof current === 'boolean'
    )
      return JSON.stringify(current);
    if (typeof current === 'number') {
      if (!Number.isFinite(current)) throw new Error('Invalid JSON number.');
      return Object.is(current, -0) ? '-0.0' : String(current);
    }
    if (Array.isArray(current)) {
      if (space === 0)
        return `[${current.map((item) => format(item, depth + 1)).join(',')}]`;
      const indentation = ' '.repeat(space * (depth + 1));
      const closing = ' '.repeat(space * depth);
      return `[\n${current
        .map((item) => `${indentation}${format(item, depth + 1)}`)
        .join(',\n')}\n${closing}]`;
    }
    if (typeof current === 'object') {
      const entries = Object.entries(current as Record<string, unknown>);
      if (space === 0)
        return `{${entries
          .map(
            ([key, item]) =>
              `${JSON.stringify(key)}:${format(item, depth + 1)}`,
          )
          .join(',')}}`;
      const indentation = ' '.repeat(space * (depth + 1));
      const closing = ' '.repeat(space * depth);
      return `{\n${entries
        .map(
          ([key, item]) =>
            `${indentation}${JSON.stringify(key)}: ${format(item, depth + 1)}`,
        )
        .join(',\n')}\n${closing}}`;
    }
    throw new Error('This value cannot be displayed as JSON.');
  }
  return format(value, 0);
}

async function readAssistantJson(response: Response) {
  return parseAssistantJson(await response.text());
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
    inspect?: (response: Response) => void,
  ): Promise<T> {
    const guidance =
      init.method === 'POST' &&
      (url === '/api/v1/runs' || /^\/api\/v1\/runs\/[^/]+\/rerun$/.test(url))
        ? 'The submission may have gone through. Check the run list and reconcile before submitting again.'
        : init.method === 'POST' && url === '/api/v1/batches'
          ? 'The batch may have been recorded. Retry with the same key or check Jobs before submitting again.'
          : init.method === 'POST' && url === '/api/v1/workflows/import/confirm'
            ? 'The workflow may have been written. Send the same confirmation again: an identical repeat is safe.'
            : init.method === 'POST' && url === '/api/v1/models/llm/pulls'
              ? 'The pull may have started. Check the list of pulls before starting it again.'
              : 'Check that the local GenAImage server is running and up to date.';
    let response: Response;
    try {
      init.signal?.throwIfAborted();
      response = await fetcher(url, { ...init, redirect: 'error' });
    } catch (error) {
      preserveAbort(error, init.signal);
      throw new ApiNetworkError(guidance, error);
    }
    inspect?.(response);
    if (!response.ok) {
      const contentType = response.headers
        .get('content-type')
        ?.split(';')[0]
        .trim()
        .toLowerCase();
      if (contentType === 'application/problem+json') {
        const problem: unknown = await response
          .text()
          .then(parseAssistantJson)
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
    doctor: (options: RequestOptions = {}) =>
      json('get', '/api/v1/doctor', options),
    setupStatus: (options: RequestOptions = {}) =>
      json('get', '/api/v1/setup/status', options),
    setupDetect: (options: RequestOptions = {}) =>
      json('post', '/api/v1/setup/detect', options),
    llmModels: (options: RequestOptions = {}) =>
      json('get', '/api/v1/models/llm', options),
    checkpoints: (options: RequestOptions = {}) =>
      json('get', '/api/v1/models/checkpoints', options),
    listModelPulls: (options: RequestOptions = {}) =>
      json('get', '/api/v1/models/llm/pulls', options),
    getModelPull: (id: string, options: RequestOptions = {}) =>
      json('get', '/api/v1/models/llm/pulls/{id}', {
        ...options,
        path: { id },
      }),
    /** One POST, never retried. A 409 problem is thrown with the existing pull's ID in `location`. */
    startModelPull: async (name: string, options: RequestOptions = {}) => {
      const seen: { location?: string | null } = {};
      try {
        const pull = await request(
          apiUrl('/api/v1/models/llm/pulls'),
          {
            method: 'POST',
            headers: {
              Accept: 'application/json, application/problem+json',
              'Content-Type': 'application/json',
            },
            body: JSON.stringify({ name }),
            signal: options.signal,
          },
          (response) =>
            response.json() as Promise<components['schemas']['ModelPull']>,
          (response) => {
            seen.location = response.headers.get('location');
          },
        );
        return { pull, existingId: undefined };
      } catch (error) {
        const id = seen.location?.split('/').filter(Boolean).at(-1);
        if (error instanceof ApiProblemError && error.status === 409 && id)
          return { pull: undefined, existingId: decodeURIComponent(id) };
        throw error;
      }
    },
    cancelModelPull: (id: string, options: RequestOptions = {}) =>
      request(
        apiUrl('/api/v1/models/llm/pulls/{id}', { id }),
        {
          method: 'DELETE',
          headers: { Accept: 'application/json, application/problem+json' },
          signal: options.signal,
        },
        (response) =>
          response.json() as Promise<components['schemas']['ModelPull']>,
      ),
    streamModelPull: (id: string, options: RequestOptions = {}) =>
      request(
        apiUrl('/api/v1/models/llm/pulls/{id}/events', { id }),
        {
          method: 'GET',
          headers: { Accept: 'text/event-stream, application/problem+json' },
          signal: options.signal,
        },
        async (response) => {
          const { readSseJson } = await import('./sse');
          return readSseJson<components['schemas']['ModelPull']>(
            response,
            options.signal,
          );
        },
      ),
    promptAssist: (body: PromptAssistRequest, options?: RequestOptions) =>
      json('post', '/api/v1/assistant/prompt', { ...options, body }),
    listTemplates: (options: RequestOptions = {}) =>
      json('get', '/api/v1/templates', options),
    /** Read-only: renders the reviewed version (`sha256`) and records nothing. */
    renderTemplate: (body: TemplateRenderRequest, options?: RequestOptions) =>
      json('post', '/api/v1/templates/render', { ...options, body }),
    suggestWorkflow: (body: SuggestWorkflowRequest, options?: RequestOptions) =>
      json(
        'post',
        '/api/v1/assistant/suggest',
        { ...options, body },
        (response) => readExactIntegers(response, true),
      ),
    listAssistantConversations: (
      query?: AssistantConversationQuery,
      options?: RequestOptions,
    ) =>
      json('get', '/api/v1/assistant/conversations', {
        ...options,
        query,
      }),
    createAssistantConversation: (
      body: JsonBody<Operation<'/api/v1/assistant/conversations', 'post'>>,
      options?: RequestOptions,
    ) =>
      json('post', '/api/v1/assistant/conversations', {
        ...options,
        body,
      }),
    getAssistantConversation: (
      id: string,
      messageLimit = 100,
      options?: RequestOptions,
    ) =>
      json(
        'get',
        '/api/v1/assistant/conversations/{id}',
        {
          ...options,
          path: { id },
          query: { message_limit: messageLimit },
        },
        (response) => readAssistantJson(response),
      ),
    deleteAssistantConversation: (id: string, options?: RequestOptions) =>
      request(
        apiUrl('/api/v1/assistant/conversations/{id}', { id }),
        { method: 'DELETE', signal: options?.signal },
        async () => undefined,
      ),
    cancelAssistantConversation: (id: string, options?: RequestOptions) =>
      request(
        apiUrl('/api/v1/assistant/conversations/{id}/cancel', { id }),
        { method: 'POST', signal: options?.signal },
        async () => undefined,
      ),
    sendAssistantMessage: (
      id: string,
      body: SendAssistantMessageRequest,
      options?: RequestOptions,
    ) =>
      request<AssistantStream>(
        apiUrl('/api/v1/assistant/conversations/{id}/messages', { id }),
        {
          method: 'POST',
          headers: {
            Accept: 'text/event-stream, application/problem+json',
            'Content-Type': 'application/json',
          },
          body: JSON.stringify(body),
          signal: options?.signal,
        },
        async (response) => {
          const { readSseJson } = await import('./sse');
          return readSseJson<AssistantStreamPayload>(
            response,
            options?.signal,
            parseAssistantJson,
          );
        },
      ),
    previewAssistantConfirmation: (
      id: string,
      confirmationId: string,
      body: JsonBody<
        Operation<
          '/api/v1/assistant/conversations/{id}/confirmations/{confirmation_id}/preview',
          'post'
        >
      >,
      options?: RequestOptions,
    ) =>
      json(
        'post',
        '/api/v1/assistant/conversations/{id}/confirmations/{confirmation_id}/preview',
        {
          ...options,
          path: { id, confirmation_id: confirmationId },
          body,
        },
        (response) => readAssistantJson(response),
      ),
    decideAssistantConfirmation: (
      id: string,
      confirmationId: string,
      body: JsonBody<
        Operation<
          '/api/v1/assistant/conversations/{id}/confirmations/{confirmation_id}',
          'post'
        >
      >,
      options?: RequestOptions,
    ) =>
      request<AssistantStream>(
        apiUrl(
          '/api/v1/assistant/conversations/{id}/confirmations/{confirmation_id}',
          { id, confirmation_id: confirmationId },
        ),
        {
          method: 'POST',
          headers: {
            Accept: 'text/event-stream, application/problem+json',
            'Content-Type': 'application/json',
          },
          body: stringifyBindingJson(body),
          signal: options?.signal,
        },
        async (response) => {
          const { readSseJson } = await import('./sse');
          return readSseJson<AssistantStreamPayload>(
            response,
            options?.signal,
            parseAssistantJson,
          );
        },
      ),
    listAssistantToolCalls: (
      id: string,
      limit = 200,
      options?: RequestOptions,
    ) =>
      json(
        'get',
        '/api/v1/assistant/conversations/{id}/tool-calls',
        {
          ...options,
          path: { id },
          query: { limit },
        },
        (response) => readAssistantJson(response),
      ),
    listWorkflows: (options: RequestOptions = {}) =>
      json('get', '/api/v1/workflows', options),
    refreshWorkflows: (options: RequestOptions = {}) =>
      json('post', '/api/v1/workflows/refresh', options),
    /** Read-only analysis. Every number in the response keeps its source text. */
    importWorkflow: (
      body: WorkflowImportRequest,
      options: RequestOptions = {},
    ) =>
      json(
        'post',
        '/api/v1/workflows/import',
        { ...options, body },
        readAssistantJson,
      ) as Promise<unknown>,
    /** One write. An identical repeat replays 200, so a retry re-sends the same body. */
    confirmWorkflowImport: (
      body: WorkflowImportConfirmRequest,
      options: RequestOptions = {},
    ) => json('post', '/api/v1/workflows/import/confirm', { ...options, body }),
    listPacks: (options: RequestOptions = {}) =>
      json('get', '/api/v1/packs', options),
    validatePack: (id: string, options: RequestOptions = {}) =>
      json('get', '/api/v1/packs/{id}/validate', {
        ...options,
        path: { id },
      }),
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
    /** Read-only. Numbers keep their wire spelling, so `2.0` and big seeds stay exact. */
    previewBatch: async (
      body: SubmitBatchRequest,
      options?: RequestOptions,
    ) => {
      requireExactNumberReads();
      return json(
        'post',
        '/api/v1/batches/preview',
        { ...options, body },
        (response) => readExactIntegers(response, true),
      ) as Promise<components['schemas']['BatchPreviewResponse']>;
    },
    /** One POST per reviewed request: a retry re-sends the identical body with the same key. */
    createBatch: async (
      body: SubmitBatchRequest,
      idempotencyKey: string,
      options?: RequestOptions,
    ) => {
      requireExactNumberReads();
      return json(
        'post',
        '/api/v1/batches',
        {
          ...options,
          body,
          header: { 'Idempotency-Key': idempotencyKey },
        },
        (response) => readExactIntegers(response, true),
      ) as Promise<components['schemas']['BatchResponse']>;
    },
    listBatches: (query?: BatchQuery, options?: RequestOptions) =>
      json('get', '/api/v1/batches', { ...options, query }),
    getBatch: (id: string, options?: RequestOptions) =>
      json(
        'get',
        '/api/v1/batches/{id}',
        { ...options, path: { id } },
        (response) => readExactIntegers(response, true),
      ) as Promise<components['schemas']['BatchResponse']>,
    resumeBatch: (id: string, options?: RequestOptions) =>
      json(
        'post',
        '/api/v1/batches/{id}/resume',
        { ...options, path: { id } },
        (response) => readExactIntegers(response, true),
      ) as Promise<components['schemas']['BatchResponse']>,
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
      request(
        apiUrl('/api/v1/runs/{id}/resolved-workflow', { id }),
        options ?? {},
        async (response) => ({
          graph: await readExactIntegers(response),
          integrity: response.headers.get('x-genaimage-workflow-integrity'),
          etag: response.headers.get('etag'),
        }),
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
    getOutputAnnotations: (id: string, options?: RequestOptions) =>
      json('get', '/api/v1/outputs/{id}/annotations', {
        ...options,
        path: { id },
      }),
    reviewOutput: (id: string, options?: RequestOptions) =>
      json('post', '/api/v1/outputs/{id}/review', {
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
