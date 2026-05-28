/**
 * RuntimeSelector — compact runtime switcher for the chat input bar.
 *
 * Sits next to the ModelSelector. Visually identical trigger style.
 * Locked (disabled) once the session has any dialog turns.
 * Falls back to a hardcoded BitFun Native entry if backend invoke fails.
 */

import React, { useEffect, useState, useRef, useCallback, useMemo } from 'react';
import { Check, ChevronDown, Cpu } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import './RuntimeSelector.scss';

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

interface AgentRuntimeDto {
  id: string;
  displayName: string;
  description: string;
  available: boolean;
  error: string | null;
  supportsSteer: boolean;
  supportsThinking: boolean;
  autonomousTools: boolean;
}

export interface RuntimeSelectorProps {
  /** Currently selected runtime id */
  selectedRuntimeId: string | null;
  /** Callback when user picks a different runtime */
  onRuntimeChange: (runtimeId: string) => void;
  /** If true, the selector is locked */
  disabled?: boolean;
}

// Hardcoded fallback — always available
const BITFUN_RUNTIME: AgentRuntimeDto = {
  id: 'bitfun',
  displayName: 'BitFun Native',
  description: "BitFun's built-in agent runtime",
  available: true,
  error: null,
  supportsSteer: false,
  supportsThinking: true,
  autonomousTools: false,
};

// ---------------------------------------------------------------------------
// Component
// ---------------------------------------------------------------------------

export const RuntimeSelector: React.FC<RuntimeSelectorProps> = ({
  selectedRuntimeId,
  onRuntimeChange,
  disabled = false,
}) => {
  const [runtimes, setRuntimes] = useState<AgentRuntimeDto[]>([BITFUN_RUNTIME]);
  const [loading, setLoading] = useState(true);
  const [dropdownOpen, setDropdownOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  // Fetch runtimes from backend
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const data = await invoke<AgentRuntimeDto[]>('list_agent_runtimes');
        console.log('[RuntimeSelector] list_agent_runtimes returned:', JSON.stringify(data));
        if (cancelled) return;
        if (data.length > 0) {
          setRuntimes(data);
        } else {
          // Backend returned empty — use fallback
          setRuntimes([BITFUN_RUNTIME]);
        }
      } catch (err) {
        console.warn('[RuntimeSelector] invoke failed, using fallback:', err);
        if (cancelled) return;
        // keep the initial [BITFUN_RUNTIME] state
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => { cancelled = true; };
  }, []);

  // Auto-select default runtime if none selected
  useEffect(() => {
    if (selectedRuntimeId === null && !loading) {
      const defaultRt = runtimes.find((r) => r.id === 'omp' && r.available) ?? runtimes.find((r) => r.available) ?? runtimes[0];
      if (defaultRt) {
        onRuntimeChange(defaultRt.id);
      }
    }
  }, [loading, runtimes, selectedRuntimeId, onRuntimeChange]);

  // Click outside
  useEffect(() => {
    if (!dropdownOpen) return;
    const handler = (e: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(e.target as Node)) {
        setDropdownOpen(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [dropdownOpen]);

  const selectedRuntime = useMemo(
    () => runtimes.find((r) => r.id === selectedRuntimeId) ?? null,
    [runtimes, selectedRuntimeId],
  );

  const handleSelect = useCallback(
    (runtimeId: string) => {
      const rt = runtimes.find((r) => r.id === runtimeId);
      if (rt) {
        onRuntimeChange(runtimeId);
        setDropdownOpen(false);
      }
    },
    [runtimes, onRuntimeChange],
  );

  const triggerLabel = loading ? '…' : selectedRuntime?.displayName ?? 'BitFun Native';

  return (
    <div ref={containerRef} className="bitfun-runtime-selector">
      <button
        className={`bitfun-runtime-selector__trigger${dropdownOpen ? ' bitfun-runtime-selector__trigger--open' : ''}`}
        onClick={() => { if (!disabled) setDropdownOpen(!dropdownOpen); }}
        disabled={disabled || loading}
        title={selectedRuntime?.description || 'BitFun Native'}
        type="button"
      >
        <Cpu size={10} className="bitfun-runtime-selector__icon" />
        <span className="bitfun-runtime-selector__name">{triggerLabel}</span>
        {!disabled && <ChevronDown size={10} className="bitfun-runtime-selector__chevron" />}
      </button>

      {dropdownOpen && !disabled && (
        <div className="bitfun-runtime-selector__dropdown">
          <div className="bitfun-runtime-selector__dropdown-header">
            <span>Runtime</span>
          </div>
          <div className="bitfun-runtime-selector__list">
            {runtimes.map((rt) => {
              const isSelected = rt.id === selectedRuntimeId;
              return (
                <button
                  key={rt.id}
                  className={`bitfun-runtime-selector__option${isSelected ? ' bitfun-runtime-selector__option--selected' : ''}${!rt.available ? ' bitfun-runtime-selector__option--unavailable' : ''}`}
                  onClick={() => handleSelect(rt.id)}
                  title={rt.error || rt.description}
                  type="button"
                >
                  <div className="bitfun-runtime-selector__option-main">
                    <span
                      className={`bitfun-runtime-selector__status-dot${rt.available ? ' bitfun-runtime-selector__status-dot--ok' : ' bitfun-runtime-selector__status-dot--err'}`}
                    />
                    <span className="bitfun-runtime-selector__option-name">{rt.displayName}</span>
                  </div>
                  {isSelected && <Check size={14} className="bitfun-runtime-selector__option-check" />}
                </button>
              );
            })}
          </div>
        </div>
      )}
    </div>
  );
};

RuntimeSelector.displayName = 'RuntimeSelector';
export default RuntimeSelector;
