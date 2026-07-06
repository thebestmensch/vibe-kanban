import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { ArrowSquareOutIcon } from '@phosphor-icons/react';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@vibe/ui/components/KeyboardDialog';
import { Button } from '@vibe/ui/components/Button';
import { Input } from '@vibe/ui/components/Input';
import { Label } from '@vibe/ui/components/Label';
import { create, useModal } from '@ebay/nice-modal-react';
import { defineModal } from '@/shared/lib/modals';
import { linearApi } from '@/shared/lib/api';

export interface LinkToLinearIssueDialogProps {
  projectId: string;
  issueId: string;
}

function errorMessage(err: unknown, fallback: string): string {
  return err instanceof Error && err.message ? err.message : fallback;
}

function LinkToLinearIssueContent({
  projectId,
  issueId,
}: LinkToLinearIssueDialogProps) {
  const modal = useModal();
  const { t } = useTranslation('tasks');
  const queryClient = useQueryClient();

  const linksQuery = useQuery({
    queryKey: ['linearLinks', projectId],
    queryFn: () => linearApi.listProjectLinks(projectId),
  });
  const existingLink = useMemo(
    () => linksQuery.data?.find((l) => l.issue_id === issueId) ?? null,
    [linksQuery.data, issueId]
  );

  const [identifier, setIdentifier] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ['linearLinks', projectId] });

  const handleLink = async () => {
    const value = identifier.trim();
    if (!value) return;
    setBusy(true);
    setError(null);
    try {
      await linearApi.linkIssue(issueId, { identifier: value });
      await invalidate();
      modal.hide();
    } catch (err) {
      setError(
        errorMessage(
          err,
          t('linkToLinear.errors.failed', 'Failed to link Linear issue.')
        )
      );
    } finally {
      setBusy(false);
    }
  };

  const handleUnlink = async () => {
    setBusy(true);
    setError(null);
    try {
      await linearApi.unlinkIssue(issueId);
      await invalidate();
      modal.hide();
    } catch (err) {
      setError(
        errorMessage(
          err,
          t('linkToLinear.errors.unlinkFailed', 'Failed to unlink.')
        )
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <Dialog open={modal.visible} onOpenChange={(open) => !open && modal.hide()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>
            {t('linkToLinear.title', 'Link to Linear issue')}
          </DialogTitle>
          <DialogDescription>
            {t(
              'linkToLinear.description',
              'Mirror this card to a Linear ticket. Status changes on the board push to Linear.'
            )}
          </DialogDescription>
        </DialogHeader>

        {existingLink ? (
          <div className="space-y-3">
            <div className="flex items-center justify-between gap-2 rounded-sm border border-border bg-secondary/30 px-base py-2">
              <span className="text-sm font-medium text-normal">
                {existingLink.linear_issue_identifier}
              </span>
              <a
                href={existingLink.linear_url}
                target="_blank"
                rel="noopener noreferrer"
                className="flex items-center gap-1 text-sm text-info hover:underline"
              >
                {t('linkToLinear.openInLinear', 'Open')}
                <ArrowSquareOutIcon className="size-4" weight="bold" />
              </a>
            </div>
            {error && <p className="text-sm text-destructive">{error}</p>}
          </div>
        ) : (
          <div className="space-y-2">
            <Label>{t('linkToLinear.identifierLabel', 'Linear issue')}</Label>
            <Input
              value={identifier}
              onChange={(e) => setIdentifier(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') handleLink();
              }}
              placeholder={t(
                'linkToLinear.identifierPlaceholder',
                'e.g. JM-718'
              )}
              autoFocus
            />
            {error && <p className="text-sm text-destructive">{error}</p>}
          </div>
        )}

        <DialogFooter>
          {existingLink ? (
            <>
              <Button
                variant="outline"
                onClick={() => modal.hide()}
                disabled={busy}
              >
                {t('common:buttons.cancel', 'Cancel')}
              </Button>
              <Button
                variant="destructive"
                onClick={handleUnlink}
                disabled={busy}
              >
                {t('linkToLinear.unlink', 'Unlink')}
              </Button>
            </>
          ) : (
            <>
              <Button
                variant="outline"
                onClick={() => modal.hide()}
                disabled={busy}
              >
                {t('common:buttons.cancel', 'Cancel')}
              </Button>
              <Button
                onClick={handleLink}
                disabled={busy || !identifier.trim()}
              >
                {busy
                  ? t('linkToLinear.linking', 'Linking…')
                  : t('linkToLinear.link', 'Link')}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

const LinkToLinearIssueDialogImpl = create<LinkToLinearIssueDialogProps>(
  ({ projectId, issueId }) => {
    if (!projectId || !issueId) return null;
    return <LinkToLinearIssueContent projectId={projectId} issueId={issueId} />;
  }
);

export const LinkToLinearIssueDialog = defineModal<
  LinkToLinearIssueDialogProps,
  void
>(LinkToLinearIssueDialogImpl);
