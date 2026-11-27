import { describe, expect, it } from 'vitest';
import type { components } from '../../lib/api/schema';
import { compareSources, sourceLabel, validationView } from './model';

type Validation = components['schemas']['PackValidationResponse'];

const validation = (overrides: Partial<Validation> = {}): Validation => ({
  pack_id: 'core',
  status: 'ok',
  reason: null,
  node_classes: [{ class: 'KSampler', status: 'available' }],
  models: [
    {
      folder: 'checkpoints',
      name: 'sd15.safetensors',
      status: 'available',
      loader: 'CheckpointLoaderSimple.ckpt_name',
      reason: null,
    },
  ],
  diagnostics: [],
  ...overrides,
});

describe('source labels', () => {
  it('names the user root and pack sources and orders the user root first', () => {
    expect(sourceLabel('user')).toBe('Your workflows');
    expect(sourceLabel('pack:core')).toBe('core');
    expect(sourceLabel('pack:')).toBe('Unknown source');
    expect(sourceLabel(undefined)).toBe('Unknown source');
    expect(['pack:core', 'user', 'pack:extra'].sort(compareSources)).toEqual([
      'user',
      'pack:core',
      'pack:extra',
    ]);
  });
});

describe('pack validation verdicts', () => {
  it('treats only an explicit ok with every item available as success', () => {
    expect(validationView(validation()).verdict).toBe('ok');
    expect(validationView(validation()).title).toBe('Requirements confirmed');
  });

  it('never reports success when diagnostics are present', () => {
    const view = validationView(
      validation({ diagnostics: ['Declared workflow is missing: a.json'] }),
    );
    expect(view.verdict).toBe('unknown');
    expect(view.diagnostics).toEqual(['Declared workflow is missing: a.json']);
  });

  it('reports a missing item or status as missing', () => {
    expect(
      validationView(
        validation({
          status: 'missing',
          node_classes: [{ class: 'KSampler', status: 'missing' }],
        }),
      ).verdict,
    ).toBe('missing');
    expect(
      validationView(
        validation({
          models: [
            {
              folder: 'loras',
              name: 'style.safetensors',
              status: 'missing',
              loader: 'LoraLoader.lora_name',
              reason: null,
            },
          ],
        }),
      ).models[0].label,
    ).toBe('Missing');
  });

  it('keeps unknown and unavailable results out of the success state', () => {
    const unknown = validationView(
      validation({
        status: 'unknown',
        node_classes: [{ class: 'KSampler', status: 'unknown' }],
      }),
    );
    expect(unknown.verdict).toBe('unknown');
    expect(unknown.title).toBe('Requirements not confirmed');
    expect(unknown.nodes[0].label).toBe('Unknown');

    const unavailable = validationView(
      validation({
        status: 'unavailable',
        reason: 'ComfyUI did not answer /object_info.',
      }),
    );
    expect(unavailable.verdict).toBe('unknown');
    expect(unavailable.headline).toContain('could not be checked');
    expect(unavailable.reason).toBe('ComfyUI did not answer /object_info.');
  });

  it('renders an unexpected status or item status as unknown', () => {
    const view = validationView(
      validation({
        status: 'surprise' as Validation['status'],
        node_classes: [{ class: 'KSampler', status: 'maybe' as 'available' }],
      }),
    );
    expect(view.verdict).toBe('unknown');
    expect(view.reason).toBe('Unexpected status "surprise" from GenAImage.');
    expect(view.nodes[0].label).toBe('Unknown');
  });

  it('tolerates missing optional fields without claiming success for them', () => {
    const bare = { pack_id: 'core', status: 'unknown' } as Validation;
    const view = validationView(bare);
    expect(view.verdict).toBe('unknown');
    expect(view.nodes).toEqual([]);
    expect(view.models).toEqual([]);
    expect(view.diagnostics).toEqual([]);
  });
});
