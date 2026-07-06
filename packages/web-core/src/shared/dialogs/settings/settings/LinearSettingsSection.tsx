import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { TrashIcon, CircleIcon, CheckCircleIcon } from '@phosphor-icons/react';
import type { LinearAccountView } from 'shared/types';
import { PrimaryButton } from '@vibe/ui/components/PrimaryButton';
import {
  linearApi,
  remoteProjectsApi,
  LOCAL_BOARD_ORG_ID,
} from '@/shared/lib/api';
import {
  SettingsCard,
  SettingsCheckbox,
  SettingsField,
  SettingsInput,
  SettingsSelect,
  TwoColumnPickerEmpty,
} from './SettingsComponents';

const ACCOUNTS_KEY = ['linearAccounts'];

function errorMessage(err: unknown, fallback: string): string {
  return err instanceof Error && err.message ? err.message : fallback;
}

// ============================================================================
// Accounts card — connect / list / remove Linear accounts (credentials).
// ============================================================================

function LinearAccountsCard() {
  const { t } = useTranslation('settings');
  const queryClient = useQueryClient();
  const accountsQuery = useQuery({
    queryKey: ACCOUNTS_KEY,
    queryFn: linearApi.listAccounts,
  });

  const [key, setKey] = useState('');
  const [token, setToken] = useState('');
  const [workspaceName, setWorkspaceName] = useState('');
  const [teamId, setTeamId] = useState('');
  const [formError, setFormError] = useState<string | null>(null);

  const invalidate = () =>
    queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });

  const connect = useMutation({
    mutationFn: () =>
      linearApi.connectAccount({
        key: key.trim(),
        token: token.trim(),
        workspace_name: workspaceName.trim() || null,
        team_id: teamId.trim() || null,
      }),
    onSuccess: () => {
      setKey('');
      setToken('');
      setWorkspaceName('');
      setTeamId('');
      setFormError(null);
      invalidate();
    },
    onError: (err) =>
      setFormError(
        errorMessage(err, t('settings.linear.errors.connectFailed'))
      ),
  });

  const remove = useMutation({
    mutationFn: (accountKey: string) => linearApi.deleteAccount(accountKey),
    onSuccess: invalidate,
  });

  const canConnect =
    key.trim().length > 0 && token.trim().length > 0 && !connect.isPending;

  const accounts = accountsQuery.data ?? [];

  return (
    <SettingsCard
      title={t('settings.linear.accounts.title')}
      description={t('settings.linear.accounts.description')}
    >
      {accounts.length > 0 && (
        <div className="space-y-2">
          {accounts.map((account) => (
            <div
              key={account.key}
              className="flex items-center gap-3 rounded-sm border border-border bg-secondary/30 px-base py-2"
            >
              {account.has_token ? (
                <CheckCircleIcon
                  className="size-icon-sm shrink-0 text-success"
                  weight="fill"
                />
              ) : (
                <CircleIcon className="size-icon-sm shrink-0 text-low" />
              )}
              <div className="min-w-0 flex-1">
                <div className="truncate text-sm font-medium text-normal">
                  {account.key}
                </div>
                <div className="truncate text-xs text-low">
                  {[account.workspace_name, account.team_id]
                    .filter(Boolean)
                    .join(' · ') || t('settings.linear.accounts.noMetadata')}
                </div>
              </div>
              <button
                type="button"
                onClick={() => remove.mutate(account.key)}
                disabled={remove.isPending}
                className="shrink-0 rounded-sm p-1 text-low transition-colors hover:bg-error/10 hover:text-error disabled:opacity-50"
                title={t('settings.linear.accounts.remove')}
                aria-label={t('settings.linear.accounts.remove')}
              >
                <TrashIcon className="size-icon-xs" weight="bold" />
              </button>
            </div>
          ))}
        </div>
      )}

      <div className="space-y-4 rounded-sm border border-dashed border-border p-4">
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2">
          <SettingsField label={t('settings.linear.accounts.keyLabel')}>
            <SettingsInput
              value={key}
              onChange={setKey}
              placeholder={t('settings.linear.accounts.keyPlaceholder')}
            />
          </SettingsField>
          <SettingsField label={t('settings.linear.accounts.tokenLabel')}>
            <SettingsInput
              value={token}
              onChange={setToken}
              placeholder={t('settings.linear.accounts.tokenPlaceholder')}
            />
          </SettingsField>
          <SettingsField label={t('settings.linear.accounts.workspaceLabel')}>
            <SettingsInput
              value={workspaceName}
              onChange={setWorkspaceName}
              placeholder={t('settings.linear.accounts.workspacePlaceholder')}
            />
          </SettingsField>
          <SettingsField
            label={t('settings.linear.accounts.teamLabel')}
            description={t('settings.linear.accounts.teamHelper')}
          >
            <SettingsInput
              value={teamId}
              onChange={setTeamId}
              placeholder={t('settings.linear.accounts.teamPlaceholder')}
            />
          </SettingsField>
        </div>
        {formError && <p className="text-sm text-error">{formError}</p>}
        <div className="flex justify-end">
          <PrimaryButton
            value={t('settings.linear.accounts.connect')}
            onClick={() => connect.mutate()}
            disabled={!canConnect}
            actionIcon={connect.isPending ? 'spinner' : undefined}
          />
        </div>
      </div>
    </SettingsCard>
  );
}

// ============================================================================
// Project-sync card — bind a project to an account + map columns → states.
// ============================================================================

const NONE_VALUE = '__none__';

function LinearProjectSyncCard() {
  const { t } = useTranslation('settings');
  const queryClient = useQueryClient();

  const accountsQuery = useQuery({
    queryKey: ACCOUNTS_KEY,
    queryFn: linearApi.listAccounts,
  });
  const projectsQuery = useQuery({
    queryKey: ['localBoardProjects'],
    queryFn: () => remoteProjectsApi.listByOrganization(LOCAL_BOARD_ORG_ID),
  });

  const projects = useMemo(
    () =>
      [...(projectsQuery.data ?? [])].sort((a, b) =>
        a.name.localeCompare(b.name)
      ),
    [projectsQuery.data]
  );

  const [selectedProjectId, setSelectedProjectId] = useState<string | null>(
    null
  );
  useEffect(() => {
    if (!selectedProjectId && projects.length > 0) {
      setSelectedProjectId(projects[0].id);
    }
  }, [projects, selectedProjectId]);

  const bindingQuery = useQuery({
    queryKey: ['linearProjectBinding', selectedProjectId],
    queryFn: () => linearApi.getProjectBinding(selectedProjectId!),
    enabled: !!selectedProjectId,
  });
  const boundKey = bindingQuery.data?.account_key ?? null;

  const statusesQuery = useQuery({
    queryKey: ['localProjectStatuses', selectedProjectId],
    queryFn: () => remoteProjectsApi.listStatusesByProject(selectedProjectId!),
    enabled: !!selectedProjectId,
  });
  const columns = useMemo(
    () =>
      [...(statusesQuery.data ?? [])]
        .filter((s) => !s.hidden)
        .sort((a, b) => a.sort_order - b.sort_order),
    [statusesQuery.data]
  );

  const workflowQuery = useQuery({
    queryKey: ['linearWorkflowStates', boundKey],
    queryFn: () => linearApi.getWorkflowStates(boundKey!),
    enabled: !!boundKey,
  });
  const workflowStates = useMemo(
    () =>
      [...(workflowQuery.data ?? [])].sort((a, b) => a.position - b.position),
    [workflowQuery.data]
  );

  const boundAccount: LinearAccountView | undefined = useMemo(
    () => accountsQuery.data?.find((a) => a.key === boundKey),
    [accountsQuery.data, boundKey]
  );

  // Draft map: project_status.id → linear state id, seeded from the bound
  // account's persisted map (scoped to this project's columns).
  const [draftMap, setDraftMap] = useState<Record<string, string>>({});
  const seededKey = `${selectedProjectId ?? ''}:${boundKey ?? ''}`;
  const [seededFor, setSeededFor] = useState<string | null>(null);
  useEffect(() => {
    if (
      seededFor === seededKey ||
      bindingQuery.isLoading ||
      statusesQuery.isLoading
    ) {
      return;
    }
    const persisted = boundAccount?.state_map ?? {};
    const seed: Record<string, string> = {};
    for (const col of columns) {
      const mapped = persisted[col.id];
      if (mapped) seed[col.id] = mapped;
    }
    setDraftMap(seed);
    setSeededFor(seededKey);
  }, [
    seededKey,
    seededFor,
    boundAccount,
    columns,
    bindingQuery.isLoading,
    statusesQuery.isLoading,
  ]);

  const invalidateBinding = () =>
    queryClient.invalidateQueries({
      queryKey: ['linearProjectBinding', selectedProjectId],
    });

  const bind = useMutation({
    mutationFn: (accountKey: string | null) =>
      linearApi.bindProject(selectedProjectId!, { account_key: accountKey }),
    onSuccess: () => {
      setSeededFor(null);
      invalidateBinding();
    },
  });

  const saveMap = useMutation({
    mutationFn: () => {
      // Preserve other projects' entries: start from the persisted map, drop
      // every column of THIS project, then apply the draft.
      const next: Record<string, string> = {};
      for (const [k, v] of Object.entries(boundAccount?.state_map ?? {})) {
        if (v) next[k] = v;
      }
      for (const col of columns) delete next[col.id];
      for (const [statusId, stateId] of Object.entries(draftMap)) {
        if (stateId) next[statusId] = stateId;
      }
      return linearApi.setStateMap(boundKey!, { state_map: next });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
    },
  });

  // Inbound import (JM-734): whether THIS project is the bound account's import
  // target, plus the optional label filter. Binding to this project is the
  // invariant the backend requires, and this card only exposes the toggle on an
  // already-bound project — so enabling it can never violate that invariant.
  const importEnabled =
    !!boundAccount?.import_target_project_id &&
    boundAccount.import_target_project_id === selectedProjectId;
  const [draftImportOn, setDraftImportOn] = useState(false);
  const [draftLabel, setDraftLabel] = useState('');
  const [importSeededFor, setImportSeededFor] = useState<string | null>(null);
  useEffect(() => {
    if (importSeededFor === seededKey || bindingQuery.isLoading) return;
    setDraftImportOn(importEnabled);
    setDraftLabel(importEnabled ? (boundAccount?.import_label ?? '') : '');
    setImportSeededFor(seededKey);
  }, [
    seededKey,
    importSeededFor,
    importEnabled,
    boundAccount,
    bindingQuery.isLoading,
  ]);

  const saveImport = useMutation({
    mutationFn: () =>
      linearApi.setImportConfig(boundKey!, {
        import_target_project_id: draftImportOn ? selectedProjectId! : null,
        import_label:
          draftImportOn && draftLabel.trim() ? draftLabel.trim() : null,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ACCOUNTS_KEY });
    },
  });

  const importDirty =
    draftImportOn !== importEnabled ||
    (draftImportOn && draftLabel.trim() !== (boundAccount?.import_label ?? ''));

  const persistedForProject = useMemo(() => {
    const persisted = boundAccount?.state_map ?? {};
    const scoped: Record<string, string> = {};
    for (const col of columns) {
      if (persisted[col.id]) scoped[col.id] = persisted[col.id]!;
    }
    return scoped;
  }, [boundAccount, columns]);

  const isDirty = useMemo(() => {
    const keys = new Set([
      ...Object.keys(persistedForProject),
      ...Object.keys(draftMap),
    ]);
    for (const k of keys) {
      if ((persistedForProject[k] ?? '') !== (draftMap[k] ?? '')) return true;
    }
    return false;
  }, [persistedForProject, draftMap]);

  const accounts = accountsQuery.data ?? [];
  const accountOptions = [
    { value: NONE_VALUE, label: t('settings.linear.sync.unbound') },
    ...accounts.map((a) => ({ value: a.key, label: a.key })),
  ];
  const projectOptions = projects.map((p) => ({ value: p.id, label: p.name }));

  return (
    <SettingsCard
      title={t('settings.linear.sync.title')}
      description={t('settings.linear.sync.description')}
    >
      {projects.length === 0 ? (
        <TwoColumnPickerEmpty>
          {t('settings.linear.sync.noProjects')}
        </TwoColumnPickerEmpty>
      ) : (
        <>
          <SettingsField label={t('settings.linear.sync.projectLabel')}>
            <SettingsSelect
              value={selectedProjectId ?? undefined}
              options={projectOptions}
              onChange={(value) => {
                setSelectedProjectId(value);
                setSeededFor(null);
              }}
              placeholder={t('settings.linear.sync.projectPlaceholder')}
            />
          </SettingsField>

          <SettingsField
            label={t('settings.linear.sync.accountLabel')}
            description={t('settings.linear.sync.accountHelper')}
          >
            <SettingsSelect
              value={boundKey ?? NONE_VALUE}
              options={accountOptions}
              disabled={accounts.length === 0 || bind.isPending}
              onChange={(value) =>
                bind.mutate(value === NONE_VALUE ? null : value)
              }
              placeholder={t('settings.linear.sync.accountPlaceholder')}
            />
          </SettingsField>

          {boundKey && (
            <SettingsField
              label={t('settings.linear.sync.mappingLabel')}
              description={t('settings.linear.sync.mappingHelper')}
            >
              {workflowQuery.isError ? (
                <p className="text-sm text-error">
                  {errorMessage(
                    workflowQuery.error,
                    t('settings.linear.errors.workflowStatesFailed')
                  )}
                </p>
              ) : workflowQuery.isLoading || statusesQuery.isLoading ? (
                <p className="text-sm text-low">
                  {t('settings.linear.sync.loadingStates')}
                </p>
              ) : columns.length === 0 ? (
                <TwoColumnPickerEmpty>
                  {t('settings.linear.sync.noColumns')}
                </TwoColumnPickerEmpty>
              ) : (
                <div className="space-y-2">
                  {columns.map((col) => (
                    <div key={col.id} className="flex items-center gap-3">
                      <span className="w-40 shrink-0 truncate text-sm text-normal">
                        {col.name}
                      </span>
                      <div className="flex-1">
                        <SettingsSelect
                          value={draftMap[col.id] ?? NONE_VALUE}
                          options={[
                            {
                              value: NONE_VALUE,
                              label: t('settings.linear.sync.stateNone'),
                            },
                            ...workflowStates.map((s) => ({
                              value: s.id,
                              label: s.name,
                            })),
                          ]}
                          onChange={(value) =>
                            setDraftMap((prev) => {
                              const next = { ...prev };
                              if (value === NONE_VALUE) delete next[col.id];
                              else next[col.id] = value;
                              return next;
                            })
                          }
                          placeholder={t(
                            'settings.linear.sync.statePlaceholder'
                          )}
                        />
                      </div>
                    </div>
                  ))}
                  <div className="flex justify-end pt-2">
                    <PrimaryButton
                      value={t('settings.linear.sync.saveMapping')}
                      onClick={() => saveMap.mutate()}
                      disabled={!isDirty || saveMap.isPending}
                      actionIcon={saveMap.isPending ? 'spinner' : undefined}
                    />
                  </div>
                </div>
              )}
            </SettingsField>
          )}

          {boundKey && (
            <SettingsField
              label={t('settings.linear.import.label')}
              description={t('settings.linear.import.helper')}
            >
              <div className="space-y-3">
                <SettingsCheckbox
                  id="linear-import-enabled"
                  label={t('settings.linear.import.enable')}
                  description={t('settings.linear.import.enableHelper')}
                  checked={draftImportOn}
                  onChange={setDraftImportOn}
                />
                {draftImportOn && (
                  <SettingsField
                    label={t('settings.linear.import.labelFilterLabel')}
                    description={t('settings.linear.import.labelFilterHelper')}
                  >
                    <SettingsInput
                      value={draftLabel}
                      onChange={setDraftLabel}
                      placeholder={t(
                        'settings.linear.import.labelFilterPlaceholder'
                      )}
                    />
                  </SettingsField>
                )}
                <div className="flex justify-end">
                  <PrimaryButton
                    value={t('settings.linear.import.save')}
                    onClick={() => saveImport.mutate()}
                    disabled={!importDirty || saveImport.isPending}
                    actionIcon={saveImport.isPending ? 'spinner' : undefined}
                  />
                </div>
              </div>
            </SettingsField>
          )}
        </>
      )}
    </SettingsCard>
  );
}

export function LinearSettingsSection() {
  return (
    <>
      <LinearAccountsCard />
      <LinearProjectSyncCard />
    </>
  );
}
