import { cn } from '../lib/cn';
import { ArrowsClockwiseIcon, ArrowSquareOutIcon } from '@phosphor-icons/react';

export interface LinearBadgeProps {
  /** Team-key identifier, e.g. "JM-718". */
  identifier: string;
  url: string;
  /** True while a board status change is queued for an outbound push to Linear. */
  syncPending?: boolean;
  className?: string;
}

/**
 * Board badge linking a card to its mirrored Linear ticket (JM-718). Mirrors
 * `PrBadge`'s shape (an external link that stops card-click propagation); shows
 * a spinner while an outbound status sync is pending.
 */
export function LinearBadge({
  identifier,
  url,
  syncPending,
  className,
}: LinearBadgeProps) {
  return (
    <a
      href={url}
      target="_blank"
      rel="noopener noreferrer"
      onClick={(e) => e.stopPropagation()}
      title={
        syncPending
          ? `${identifier} — syncing status to Linear…`
          : `${identifier} — open in Linear`
      }
      className={cn(
        'flex items-center gap-half px-1.5 py-0.5 rounded text-xs font-medium transition-colors',
        'bg-info/10 text-info hover:bg-info/20',
        className
      )}
    >
      {syncPending ? (
        <ArrowsClockwiseIcon
          className="size-icon-2xs animate-spin"
          weight="bold"
        />
      ) : (
        <ArrowSquareOutIcon className="size-icon-2xs" weight="bold" />
      )}
      <span>{identifier}</span>
    </a>
  );
}
