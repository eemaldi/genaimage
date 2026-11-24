import { describe, expect, it, vi } from 'vitest';
import {
  ApiHttpError,
  ApiNetworkError,
  ApiProblemError,
  ApiResponseError,
  createApiClient,
  outputMediaUrl,
  outputThumbnailUrl,
  workflowThumbnailUrl,
} from './client';

const mockFetch = () => vi.fn<typeof fetch>();

describe('portal API client', () => {
  it('preserves problem details, settings errors and other extensions', async () => {
    const problem = {
      status: 422,
      type: 'about:blank',
      title: 'Unprocessable Entity',
      detail: 'Settings are invalid.',
      instance: '/api/v1/settings',
      recovery: 'Correct the highlighted settings.',
      errors: [{ key: 'endpoint', reason: 'Required' }],
      future_extension: 'preserved',
    };
    const fetcher = mockFetch().mockResolvedValue(
      new Response(JSON.stringify(problem), {
        status: 422,
        headers: { 'Content-Type': 'application/problem+json; charset=utf-8' },
      }),
    );
    const result = createApiClient(fetcher).putSettings({});
    await expect(result).rejects.toBeInstanceOf(ApiProblemError);
    const { future_extension: _extension, ...fields } = problem;
    await expect(result).rejects.toMatchObject({ ...fields, problem });
  });

  it.each(['text/html', 'application/problem+json'])(
    'maps an unusable error response (%s)',
    async (contentType) => {
      const fetcher = mockFetch().mockResolvedValue(
        new Response('<h1>Unavailable</h1>', {
          status: 503,
          headers: { 'Content-Type': contentType },
        }),
      );
      const result = createApiClient(fetcher).health();
      await expect(result).rejects.toBeInstanceOf(ApiHttpError);
      await expect(result).rejects.toMatchObject({
        status: 503,
        message: expect.stringContaining('local GenAImage server'),
      });
    },
  );

  it('does not retry an ambiguous submission and gives reconciliation guidance', async () => {
    const cause = new TypeError('Disconnected');
    const fetcher = mockFetch().mockRejectedValue(cause);
    const result = createApiClient(fetcher).createRun(
      { workflow: 'portrait' },
      'test-key',
    );
    await expect(result).rejects.toBeInstanceOf(ApiNetworkError);
    await expect(result).rejects.toMatchObject({
      cause,
      message: expect.stringContaining('reconcile'),
    });
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it('distinguishes a successful response with invalid JSON from an HTTP error', async () => {
    const fetcher = mockFetch().mockResolvedValue(
      new Response('<html></html>'),
    );
    await expect(createApiClient(fetcher).health()).rejects.toBeInstanceOf(
      ApiResponseError,
    );
  });

  it('propagates aborts during fetch and response reading, and skips pre-aborted fetches', async () => {
    const reason = new DOMException('Cancelled', 'AbortError');
    const fetcher = mockFetch().mockRejectedValueOnce(reason);
    const api = createApiClient(fetcher);
    const signal = AbortSignal.abort(reason);
    await expect(api.health({ signal })).rejects.toBe(reason);
    expect(fetcher).not.toHaveBeenCalled();
    await expect(api.health()).rejects.toBe(reason);
    const response = new Response('{}');
    vi.spyOn(response, 'json').mockRejectedValueOnce(reason);
    fetcher.mockResolvedValueOnce(response);
    await expect(api.health()).rejects.toBe(reason);
  });

  it('encodes query and path parameters and forwards the signal', async () => {
    const fetcher = mockFetch().mockResolvedValueOnce(
      Response.json({ outputs: [] }),
    );
    const signal = new AbortController().signal;
    await createApiClient(fetcher).listOutputs(
      { workflow: 'one & two', cursor: 'a+=/', limit: 0, tag: undefined },
      { signal },
    );
    expect(fetcher).toHaveBeenCalledWith(
      '/api/v1/outputs?workflow=one+%26+two&cursor=a%2B%3D%2F&limit=0',
      expect.objectContaining({ method: 'GET', signal }),
    );
    expect(outputMediaUrl('a/#?')).toBe('/api/v1/outputs/a%2F%23%3F');
    expect(outputThumbnailUrl('a/#?', 256)).toBe(
      '/api/v1/outputs/a%2F%23%3F/thumbnail?size=256',
    );
    expect(workflowThumbnailUrl('a/#?')).toBe(
      '/api/v1/workflows/a%2F%23%3F/thumbnail',
    );
  });

  it.each([200, 202])(
    'builds a keyed JSON submission and accepts HTTP %s',
    async (status) => {
      const fetcher = mockFetch().mockResolvedValueOnce(
        Response.json({ id: 'run-1' }, { status }),
      );
      const body = { workflow: 'portrait', parameters: { seed: 42 } };
      await expect(
        createApiClient(fetcher).createRun(body, 'test-key'),
      ).resolves.toEqual({ id: 'run-1' });
      const [url, init] = fetcher.mock.calls[0];
      expect(url).toBe('/api/v1/runs');
      expect(init).toMatchObject({
        method: 'POST',
        redirect: 'error',
        body: JSON.stringify(body),
      });
      expect(new Headers(init?.headers).get('Idempotency-Key')).toBe(
        'test-key',
      );
      expect(new Headers(init?.headers).get('Content-Type')).toBe(
        'application/json',
      );
    },
  );

  it.each([200, 201])(
    'sends one bodyless keyed re-run (HTTP %s)',
    async (status) => {
      const fetcher = mockFetch().mockResolvedValueOnce(
        Response.json({ id: 'new', rerun_of: 'a/#?' }, { status }),
      );
      const signal = new AbortController().signal;
      await expect(
        createApiClient(fetcher).rerunRun('a/#?', 'key', { signal }),
      ).resolves.toEqual({ id: 'new', rerun_of: 'a/#?' });
      expect(fetcher).toHaveBeenCalledTimes(1);
      const [url, init] = fetcher.mock.calls[0];
      expect(url).toBe('/api/v1/runs/a%2F%23%3F/rerun');
      expect(init).toMatchObject({ method: 'POST', redirect: 'error', signal });
      expect(init?.body).toBeUndefined();
      expect(new Headers(init?.headers).get('Idempotency-Key')).toBe('key');
      expect(new Headers(init?.headers).has('Content-Type')).toBe(false);
    },
  );

  it('does not retry a re-run after a conflict or an ambiguous network error', async () => {
    const problem = {
      status: 409,
      type: 'urn:genaimage:problem:rerun-mismatch',
      title: 'Conflict',
      detail: 'The re-run cannot be verified.',
      recovery: 'Use Remix.',
    };
    const fetcher = mockFetch()
      .mockResolvedValueOnce(
        Response.json(problem, {
          status: 409,
          headers: { 'Content-Type': 'application/problem+json' },
        }),
      )
      .mockRejectedValue(new TypeError('Disconnected'));
    const api = createApiClient(fetcher);
    await expect(api.rerunRun('original', 'key')).rejects.toMatchObject({
      problem,
    });
    expect(fetcher).toHaveBeenCalledTimes(1);
    await expect(api.rerunRun('original', 'key')).rejects.toBeInstanceOf(
      ApiNetworkError,
    );
    expect(fetcher).toHaveBeenCalledTimes(2);
  });

  it('sends uploads as multipart and lets the browser set the boundary', async () => {
    const fetcher = mockFetch().mockResolvedValueOnce(
      Response.json({ id: 'upload-1' }, { status: 201 }),
    );
    const file = new File(['fake image'], 'example.png', { type: 'image/png' });
    await createApiClient(fetcher).upload(file);
    const [url, init] = fetcher.mock.calls[0];
    expect(url).toBe('/api/v1/uploads');
    expect(init?.method).toBe('POST');
    expect((init?.body as FormData).get('file')).toBe(file);
    expect(new Headers(init?.headers).has('Content-Type')).toBe(false);
  });
});
