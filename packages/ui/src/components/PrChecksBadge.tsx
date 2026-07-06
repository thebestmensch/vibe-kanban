import {
  CheckCircleIcon,
  WarningCircleIcon,
  SpinnerGapIcon,
} from '@phosphor-icons/react';
import type { CheckStatus } from 'shared/types';
import { cn } from '../lib/cn';

export interface PrChecksBadgeProps {
  /** Aggregated CI-check status; `null`/`no_checks` renders nothing. */
  status?: CheckStatus | null;
  /** Render the text label beside the icon (default: icon only). */
  showLabel?: boolean;
  className?: string;
}

const CHECK_LABEL: Record<CheckStatus, string> = {
  passing: 'Checks passing',
  failing: 'Checks failing',
  pending: 'Checks running',
  no_checks: '',
};

/**
 * Compact CI-check status indicator for a pull request. Deliberately separate
 * from `PrBadge` (which owns open/merged/closed) — check status is an
 * orthogonal axis and each render site composes the two independently.
 *
 * Renders nothing for `no_checks` / `null` / unknown so a PR with no configured
 * checks (or a non-GitHub provider) stays clean rather than showing a false
 * "neutral" state.
 */
export function PrChecksBadge({
  status,
  showLabel = false,
  className,
}: PrChecksBadgeProps) {
  if (!status || status === 'no_checks') return null;

  const { Icon, tone, spin } =
    status === 'passing'
      ? { Icon: CheckCircleIcon, tone: 'text-success', spin: false }
      : status === 'failing'
        ? { Icon: WarningCircleIcon, tone: 'text-error', spin: false }
        : { Icon: SpinnerGapIcon, tone: 'text-normal', spin: true };

  const label = CHECK_LABEL[status];

  return (
    <span
      className={cn(
        'inline-flex items-center gap-half text-sm font-medium',
        tone,
        className
      )}
      title={label}
      aria-label={label}
    >
      <Icon
        className={cn('size-icon-xs', spin && 'animate-spin')}
        weight={spin ? 'bold' : 'fill'}
      />
      {showLabel && <span>{label}</span>}
    </span>
  );
}
