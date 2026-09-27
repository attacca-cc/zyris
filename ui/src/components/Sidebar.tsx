import {
  ActivityIcon,
  BlocksIcon,
  MessageCircleIcon,
  MicIcon,
  SettingsIcon,
  WrenchIcon,
  type LucideIcon,
} from "lucide-react";
import { SHOWN_TABS, type Screen, type State, type Tab } from "@/state";
import { cn } from "@/lib/utils";
import { Mark, Wordmark } from "./Wordmark";

const ICONS: Record<Tab, LucideIcon> = {
  conversation: MessageCircleIcon,
  voice: MicIcon,
  tools: WrenchIcon,
  mcp: BlocksIcon,
  status: ActivityIcon,
  settings: SettingsIcon,
};

// No router. There are no URLs here — the window is one process with one screen showing — so a
// router would be a dependency, a history stack and a set of paths to keep in step, in exchange
// for what an equality check already does.
//
// Below 820px it is a rail of icons with the names kept for screen readers, since 240px of a
// narrow window is too much to spend on names. Below 640px — a window docked beside another, or a
// phone — it is a bar of icons along the bottom, where a thumb reaches, and the connection line
// gives way to the Status screen.
export function Sidebar({
  screen,
  state,
  onNavigate,
}: {
  screen: Screen;
  state: State;
  onNavigate: (to: Tab) => void;
}) {
  const main = SHOWN_TABS.filter((tab) => tab.id !== "settings");
  const settings = SHOWN_TABS.find((tab) => tab.id === "settings");
  return (
    <nav
      aria-label="Screens"
      className="flex w-60 shrink-0 flex-col gap-0.5 border-r border-sidebar-border bg-sidebar px-3 pt-3.5 pb-3 max-[820px]:w-16 max-[820px]:px-2 max-sm:w-full max-sm:flex-row max-sm:border-t max-sm:border-r-0 max-sm:px-1 max-sm:pt-1 max-sm:pb-[max(0.25rem,env(safe-area-inset-bottom))]"
    >
      <div className="mb-2.5 flex h-9 items-center px-2.5 max-[820px]:justify-center max-[820px]:px-0 max-sm:hidden">
        <Wordmark className="max-[820px]:hidden" />
        <Mark className="hidden max-[820px]:block" />
      </div>
      {main.map((tab) => (
        <Item key={tab.id} tab={tab.id} label={tab.label} current={tab.id === screen} onNavigate={onNavigate} />
      ))}
      <div className="flex-1 max-sm:hidden" />
      {settings && (
        <Item tab="settings" label={settings.label} current={screen === "settings"} onNavigate={onNavigate} />
      )}
      <Connection state={state} />
    </nav>
  );
}

function Item({
  tab,
  label,
  current,
  onNavigate,
}: {
  tab: Tab;
  label: string;
  current: boolean;
  onNavigate: (to: Tab) => void;
}) {
  const Icon = ICONS[tab];
  return (
    <button
      type="button"
      // Exactly one item is current, decided by equality against the screen that is showing.
      aria-current={current ? "page" : undefined}
      title={label}
      onClick={() => onNavigate(tab)}
      className={cn(
        "flex h-[2.125rem] items-center gap-[0.6875rem] rounded-[0.4375rem] px-2.5 text-left text-[0.84375rem] transition-colors outline-none focus-visible:ring-[3px] focus-visible:ring-ring/40 max-[820px]:justify-center max-[820px]:px-0 max-sm:h-12 max-sm:flex-1",
        current
          ? "bg-[#1d1814] font-medium text-heading shadow-[inset_0_0_0_1px_#2a221d]"
          : "text-[#a8a098] hover:bg-[#15110e] hover:text-heading",
      )}
    >
      <Icon className={cn("size-[1.125rem] shrink-0", current ? "text-primary" : "text-[#7d756e]")} aria-hidden="true" />
      <span className="truncate max-[820px]:sr-only">{label}</span>
    </button>
  );
}

// One line: whether this computer is connected, and as what. The Status screen has the rest.
function Connection({ state }: { state: State }) {
  return (
    <div
      className="mt-1.5 flex h-8 items-center gap-2 max-sm:hidden border-t border-[#1d1814] px-2.5 pt-2 text-xs text-muted-foreground max-[820px]:justify-center max-[820px]:px-0"
      title={state.connected ? `Connected as ${state.node?.nodeName ?? "this computer"}` : "Not connected"}
    >
      <span
        aria-hidden="true"
        className={cn("size-1.5 shrink-0 rounded-full", state.connected ? "bg-success" : "bg-subtle")}
      />
      <span className="text-[#bdb5ad] max-[820px]:sr-only">{state.connected ? "Connected" : "Not connected"}</span>
      {state.connected && state.node && (
        <>
          <span className="text-[#4a3f37] max-[820px]:hidden" aria-hidden="true">
            ·
          </span>
          <span className="truncate font-mono text-[0.71875rem] max-[820px]:hidden">{state.node.nodeName}</span>
        </>
      )}
    </div>
  );
}
