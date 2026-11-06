import { useEffect, useRef, useState } from "react";

import {
  type DetectedApps,
  detectedApps,
  failureOf,
  loadOpenAtLogin,
  loadSettings,
  openNotificationSettings,
  saveOpenAtLogin,
  saveSettings,
  type Settings,
  setNotifyOnFailure,
} from "./settings";

/** The value of the pop-up entry that asks for a name to be typed. */
const OTHER = "__other__";

/** The value of the pop-up entry that clears the app. */
const NONE = "";

/**
 * The Settings screen. Every change applies at once, there is no Save button. The "General", "Notifications" and
 * "Open with" sections live here, the others come with the behaviour they set.
 */
export function SettingsScreen({ focusEditor }: { focusEditor: boolean }) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [apps, setApps] = useState<DetectedApps | null>(null);
  const [openAtLogin, setOpenAtLogin] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Whether the last attempt to turn "Notify on failure" on was refused, which the line below the switch says. */
  const [refused, setRefused] = useState(false);

  useEffect(() => {
    let stopped = false;
    Promise.all([loadSettings(), detectedApps(), loadOpenAtLogin()])
      .then(([loaded, detected, login]) => {
        if (!stopped) {
          setSettings(loaded);
          setApps(detected);
          setOpenAtLogin(login);
        }
      })
      .catch((reason: unknown) => {
        if (!stopped) {
          setError(failureOf(reason).message);
        }
      });
    return () => {
      stopped = true;
    };
  }, []);

  function apply(next: Settings): void {
    setSettings(next);
    setError(null);
    // The choice stays on screen when the file cannot be written: Rust holds it until the app quits.
    saveSettings(next).catch((reason: unknown) => {
      setError(failureOf(reason).message);
    });
  }

  /** Asks the system to switch Open at login, and shows the state the system ends up in, whatever was asked. */
  function switchOpenAtLogin(enabled: boolean): void {
    setError(null);
    saveOpenAtLogin(enabled)
      .then(setOpenAtLogin)
      .catch((reason: unknown) => {
        setError(failureOf(reason).message);
        loadOpenAtLogin()
          .then(setOpenAtLogin)
          .catch(() => {
            // The switch keeps what it showed, and the error above says why it did not change.
          });
      });
  }

  /**
   * Asks Rust to turn "Notify on failure" on or off. Rust asks for the permission when it is turned on, and the switch
   * shows what it ends up as, which is off when the permission was refused.
   */
  function switchNotifications(enabled: boolean): void {
    setError(null);
    setRefused(false);
    setNotifyOnFailure(enabled)
      .then((on) => {
        setSettings((current) => (current === null ? current : { ...current, notify_on_failure: on }));
        setRefused(enabled && !on);
      })
      .catch((reason: unknown) => {
        setError(failureOf(reason).message);
      });
  }

  function showNotificationSettings(): void {
    setError(null);
    openNotificationSettings().catch((reason: unknown) => {
      setError(failureOf(reason).message);
    });
  }

  return (
    <section aria-label="Settings">
      <h2 className="title">Settings</h2>
      {error === null ? null : (
        <p role="alert" className="error">
          {error}
        </p>
      )}
      {settings === null || apps === null || openAtLogin === null ? null : (
        <section aria-labelledby="general">
          <h3 id="general" className="subtitle">
            General
          </h3>
          <Switch
            label="Show in Dock while the window is open"
            checked={settings.show_in_dock}
            onChange={(show_in_dock) => {
              apply({ ...settings, show_in_dock });
            }}
          />
          <Switch label="Open at login" checked={openAtLogin} onChange={switchOpenAtLogin} />
        </section>
      )}
      {settings === null ? null : (
        <section aria-labelledby="notifications">
          <h3 id="notifications" className="subtitle">
            Notifications
          </h3>
          <Switch
            label="Notify on failure"
            checked={settings.notify_on_failure}
            onChange={switchNotifications}
          />
          <p className="caption">
            A banner tells you when a failure enters Needs you while Procly is in the background. macOS
            decides whether the banner shows: allow Procly in System Settings &gt; Notifications.
          </p>
          {refused ? (
            <p role="alert" className="error">
              Notifications are not allowed for Procly, so Notify on failure stays off. Allow them in System
              Settings &gt; Notifications, then turn it on again.
            </p>
          ) : null}
          <button type="button" className="button" onClick={showNotificationSettings}>
            Open notification settings
          </button>
        </section>
      )}
      {settings === null || apps === null ? null : (
        <section aria-labelledby="open-with">
          <h3 id="open-with" className="subtitle">
            Open with
          </h3>
          {apps.editors.length === 0 && apps.terminals.length === 0 ? (
            <p className="caption">
              No installed app was detected, so each field takes the name of an app as text. Outside macOS
              this is not guaranteed.
            </p>
          ) : null}
          <AppField
            label="Editor"
            value={settings.editor}
            detected={apps.editors}
            allowNone
            focus={focusEditor}
            onChange={(editor) => {
              apply({ ...settings, editor });
            }}
          />
          <AppField
            label="Terminal"
            value={settings.terminal}
            detected={apps.terminals}
            allowNone={false}
            focus={false}
            onChange={(terminal) => {
              // A terminal cannot be none: a cleared name keeps the one in use.
              if (terminal !== null) {
                apply({ ...settings, terminal });
              }
            }}
          />
        </section>
      )}
    </section>
  );
}

/** A setting that is on or off, drawn as a checkbox that reads as a switch to a screen reader. */
function Switch({
  label,
  checked,
  onChange,
}: {
  label: string;
  checked: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <label className="switch">
      <input
        type="checkbox"
        role="switch"
        checked={checked}
        onChange={(event) => {
          onChange(event.target.checked);
        }}
      />
      {label}
    </label>
  );
}

/**
 * One app: a pop-up of the detected ones and "Other…", which takes any app name as free text. Where nothing is
 * detected, as on Linux, it is the free text alone.
 */
function AppField({
  label,
  value,
  detected,
  allowNone,
  focus,
  onChange,
}: {
  label: string;
  value: string | null;
  detected: string[];
  allowNone: boolean;
  focus: boolean;
  onChange: (value: string | null) => void;
}) {
  const [other, setOther] = useState(false);
  const [draft, setDraft] = useState(value ?? "");
  const popup = useRef<HTMLSelectElement>(null);
  const text = useRef<HTMLInputElement>(null);

  // The field the developer was sent to is ready to be changed.
  useEffect(() => {
    if (focus) {
      (text.current ?? popup.current)?.focus();
    }
  }, [focus]);

  const typed = detected.length === 0 || other || (value !== null && !detected.includes(value));

  function type(name: string): void {
    setDraft(name);
    if (name.trim() !== "") {
      onChange(name);
    } else if (allowNone) {
      onChange(null);
    }
  }

  return (
    <div className="field">
      <label className="caption" htmlFor={`${label}-app`}>
        {label}
      </label>
      {detected.length === 0 ? null : (
        <select
          id={`${label}-app`}
          ref={popup}
          className="control"
          value={typed ? OTHER : (value ?? NONE)}
          onChange={(event) => {
            const choice = event.target.value;
            if (choice === OTHER) {
              setOther(true);
              setDraft("");
              return;
            }
            setOther(false);
            onChange(choice === NONE ? null : choice);
          }}
        >
          {allowNone ? <option value={NONE}>None</option> : null}
          {detected.map((name) => (
            <option key={name} value={name}>
              {name}
            </option>
          ))}
          <option value={OTHER}>Other…</option>
        </select>
      )}
      {typed ? (
        <input
          id={detected.length === 0 ? `${label}-app` : undefined}
          ref={text}
          type="text"
          className="control"
          aria-label={`${label} app name`}
          placeholder="App name"
          spellCheck={false}
          value={draft}
          onChange={(event) => {
            type(event.target.value);
          }}
        />
      ) : null}
    </div>
  );
}
