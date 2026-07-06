import { describe, it, expect, beforeEach, vi } from 'vitest';

// Mock the transport so the client methods can be exercised without a browser
// or a live server — assert the URL/method/body they build and that the parsed
// `data` flows back through `handleApiResponse`.
const { makeLocalApiRequest } = vi.hoisted(() => ({
  makeLocalApiRequest: vi.fn(),
}));
vi.mock('@/shared/lib/localApiTransport', () => ({ makeLocalApiRequest }));

import { remoteProjectsApi } from './api';

function okJson(data: unknown): Response {
  return {
    ok: true,
    status: 200,
    json: async () => ({ success: true, data }),
  } as unknown as Response;
}

describe('remoteProjectsApi claude-variant (JM-735)', () => {
  beforeEach(() => makeLocalApiRequest.mockReset());

  it('getClaudeVariant GETs the URL-encoded project path and returns the view', async () => {
    makeLocalApiRequest.mockResolvedValue(okJson({ variant: 'WORK' }));

    const result = await remoteProjectsApi.getClaudeVariant('proj/1');

    const [url, init] = makeLocalApiRequest.mock.calls[0];
    expect(url).toBe('/api/remote/projects/proj%2F1/claude-variant');
    expect(init?.method ?? 'GET').toBe('GET');
    expect(result).toEqual({ variant: 'WORK' });
  });

  it('setClaudeVariant PUTs the variant body and returns the view', async () => {
    makeLocalApiRequest.mockResolvedValue(okJson({ variant: 'PERSONAL' }));

    const result = await remoteProjectsApi.setClaudeVariant('p1', 'PERSONAL');

    const [url, init] = makeLocalApiRequest.mock.calls[0];
    expect(url).toBe('/api/remote/projects/p1/claude-variant');
    expect(init?.method).toBe('PUT');
    expect(JSON.parse(init?.body as string)).toEqual({ variant: 'PERSONAL' });
    expect(result).toEqual({ variant: 'PERSONAL' });
  });

  it('setClaudeVariant serializes a null variant (clear the binding)', async () => {
    makeLocalApiRequest.mockResolvedValue(okJson({ variant: null }));

    const result = await remoteProjectsApi.setClaudeVariant('p1', null);

    const [, init] = makeLocalApiRequest.mock.calls[0];
    expect(JSON.parse(init?.body as string)).toEqual({ variant: null });
    expect(result).toEqual({ variant: null });
  });
});
