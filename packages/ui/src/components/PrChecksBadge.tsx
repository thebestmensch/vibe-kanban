import {
  CheckCircleIcon,
  WarningCircleIcon,
  SpinnerGapIcon,
} from '@phosphor-icons/react';
import { useTranslation } from 'react-i18next';
import type { CheckStatus } from 'shared/types';
import { cn } from '../lib/cn';

export interface PrChecksBadgeProps {
  /** Aggregated CI-check status; `null`/`no_checks` renders nothing. */
  status?: CheckStatus | null;
  /** Render the text label beside the icon (default: icon only). */
  showLabel?: boolean;
  className?: string;
}

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
  const { t } = useTranslation('tasks');
  if (!status || status === 'no_checks') return null;

  // Switch (not a ternary chain) so a future `CheckStatus` variant is caught at
  // compile time via the `never` guard instead of silently rendering as pending.
  const { Icon, tone, spin, label } = (() => {
    switch (status) {
      case 'passing':
        return {
          Icon: CheckCircleIcon,
          tone: 'text-success',
          spin: false,
          label: t('git.checks.passing'),
        };
      case 'failing':
        return {
          Icon: WarningCircleIcon,
          tone: 'text-error',
          spin: false,
          label: t('git.checks.failing'),
        };
      case 'pending':
        return {
          Icon: SpinnerGapIcon,
          tone: 'text-normal',
          spin: true,
          label: t('git.checks.pending'),
        };
      default: {
        const _exhaustive: never = status;
        return _exhaustive;
      }
    }
  })();

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
