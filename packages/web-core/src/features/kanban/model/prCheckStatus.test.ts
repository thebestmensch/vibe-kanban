import { describe, it, expect } from 'vitest';
import { getPrCheckStatus } from './prCheckStatus';

describe('getPrCheckStatus', () => {
  it('returns the check_status when present (local fallback row)', () => {
    expect(getPrCheckStatus({ check_status: 'passing' })).toBe('passing');
    expect(getPrCheckStatus({ check_status: 'failing' })).toBe('failing');
    expect(getPrCheckStatus({ check_status: 'no_checks' })).toBe('no_checks');
  });

  it('returns null when the field is absent (remote/Electric row)', () => {
    expect(getPrCheckStatus({ id: 'x', number: 1 })).toBeNull();
  });

  it('returns null when check_status is explicitly null', () => {
    expect(getPrCheckStatus({ check_status: null })).toBeNull();
  });
});
