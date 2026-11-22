import { Loader, Textarea, Tooltip } from "@mantine/core";
import { IconArrowUp, IconPlayerStopFilled } from "@tabler/icons-react";
import { useRef, type RefObject } from "react";
import type { AgentKind, ChatConfigOption, ChatControls, ChatPermissionMode } from "../../services/ipc/generated/core";
import type { QueuedMessage } from "../../lib/agentControls";
import { AttachmentList, AttachmentPicker, clipboardFiles, type useChatAttachments } from "../ChatAttachments";
import { ModelPicker } from "../ModelPicker";
import { PermissionPicker } from "../PermissionPicker";
import { ContextMeter, OptionPicker, QueuedMessages } from "./AgentControls";
import { ComposerMenu } from "./ComposerMenu";
import type { useComposerMenu } from "./useComposerMenu";
import type { useMessageQueue } from "./useMessageQueue";

/** Send, queue or stop: one round button whose glyph turns into the next action. */
function ActionButton({ mode, label, loading, disabled, onClick }: {
  mode: "send" | "stop";
  label: string;
  loading?: boolean;
  disabled?: boolean;
  onClick: () => void;
}) {
  const stop = mode === "stop";
  return (
    <Tooltip label={label} openDelay={300}>
      <span className="inline-flex">
        <button
          type="button"
          aria-label={label}
          disabled={disabled}
          onClick={onClick}
          className={`press grid size-8 cursor-pointer place-items-center rounded-full border-0 p-0 disabled:cursor-default ${
            stop
              ? "bg-hover text-fg hover:bg-sel"
              : "bg-accent text-white hover:bg-accent/85 disabled:bg-hover disabled:text-faint"
          }`}
        >
          {loading ? <Loader size={14} color="white" /> : (
            <span key={mode} className="grid animate-settle place-items-center">
              {stop ? <IconPlayerStopFilled size={13} /> : <IconArrowUp size={16} stroke={2.4} />}
            </span>
          )}
        </button>
      </span>
    </Tooltip>
  );
}

/**
 * The message box: queue above, `/` and `@` menus, the text, attachments,
 * then one bar with the session's choices on the left and send/stop on the
 * right. Enter sends (or queues while the agent works), Shift+Enter breaks a
 * line, Esc stops the turn, ↑ in an empty box recalls the last prompt.
 */
export function ChatComposer({
  input, text, onText, onCaret, placeholder, docked, busy, sending, canSend, canSteer,
  menu, queue, attachments, onImages, onSend, onStop, onRecall,
  agent, workspaceId, model, live, reasoning, otherOptions, optionsDisabled, permission, controls,
  onModel, onReasoning, onPermission, onOption,
}: {
  input: RefObject<HTMLTextAreaElement | null>;
  text: string;
  onText: (text: string) => void;
  onCaret: (caret: number) => void;
  placeholder: string;
  /** A request or plan card sits right above the box. */
  docked: boolean;
  busy: boolean;
  sending: boolean;
  canSend: boolean;
  canSteer: boolean;
  menu: ReturnType<typeof useComposerMenu>;
  queue: ReturnType<typeof useMessageQueue> & { onEdit: (message: QueuedMessage) => void };
  attachments: ReturnType<typeof useChatAttachments>;
  onImages: (files: File[]) => void;
  onSend: () => void;
  onStop: () => void;
  /** The last prompt's text, for ↑ in an empty box. */
  onRecall: () => string | null;
  agent: AgentKind;
  workspaceId: string;
  model: string | null;
  live: ChatConfigOption | null;
  reasoning: ChatConfigOption | null;
  otherOptions: ChatConfigOption[];
  optionsDisabled: boolean;
  permission: ChatPermissionMode;
  controls: ChatControls;
  onModel: (model: string) => Promise<void>;
  onReasoning?: (value: string) => Promise<void>;
  onPermission: (mode: ChatPermissionMode) => Promise<void>;
  onOption: (id: string, value: string) => Promise<void>;
}) {
  const composing = useRef(false);
  const hasDraft = text.trim().length > 0 || attachments.files.length > 0;

  const images = controls.capabilities.prompt.images;
  return (
    <div
      className={`glass relative rounded-[var(--card-radius)] px-3 pt-2.5 pb-2 transition-colors duration-[var(--motion-fast)] focus-within:border-line ${docked ? "rounded-t-none border-t-0" : ""}`}
      onDragOver={(event) => { if (event.dataTransfer.types.includes("Files")) event.preventDefault(); }}
      onDrop={(event) => {
        const files = clipboardFiles(event.dataTransfer);
        if (!files.length) return;
        event.preventDefault();
        onImages(files);
      }}
    >
      <QueuedMessages
        queue={queue.queue}
        canSteer={canSteer}
        onSteer={(message) => void queue.steer(message)}
        onEdit={queue.onEdit}
        onMove={queue.move}
        onRemove={queue.remove}
      />
      {menu.open && menu.trigger && (
        <ComposerMenu
          trigger={menu.trigger}
          items={menu.items}
          active={menu.index}
          loading={menu.loading}
          error={menu.error}
          onHover={menu.setIndex}
          onPick={menu.pick}
        />
      )}
      <Textarea
        ref={input}
        autoFocus
        variant="unstyled"
        autosize
        minRows={1}
        maxRows={10}
        value={text}
        onChange={(event) => { onText(event.currentTarget.value); onCaret(event.currentTarget.selectionStart ?? 0); }}
        onSelect={(event) => onCaret(event.currentTarget.selectionStart ?? 0)}
        placeholder={placeholder}
        aria-label="Message"
        classNames={{ input: "!min-h-10 !border-0 !bg-transparent !px-0.5 !py-0.5 !text-[14.5px] !leading-normal !shadow-none !outline-none" }}
        onCompositionStart={() => { composing.current = true; }}
        onCompositionEnd={() => { composing.current = false; }}
        onPaste={(event) => {
          const files = clipboardFiles(event.clipboardData);
          if (!files.length) return;
          event.preventDefault();
          onImages(files);
        }}
        onKeyDown={(event) => {
          const ime = composing.current || event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229;
          if (!ime && menu.onKey(event)) return;
          if (event.key === "Enter" && !event.shiftKey && !ime) {
            event.preventDefault();
            onSend();
          } else if (event.key === "Escape" && busy) {
            event.preventDefault(); // Esc stops the turn, as in the agent CLIs.
            onStop();
          } else if (event.key === "ArrowUp" && !text && !ime) {
            const previous = onRecall();
            if (previous) { event.preventDefault(); onText(previous); }
          }
        }}
      />
      <AttachmentList state={attachments} />
      <div className="mt-1 grid grid-cols-[minmax(0,1fr)_auto] items-end gap-2">
        <div className="flex min-w-0 flex-wrap items-center gap-0.5 [&>*]:max-w-full">
          {images && <AttachmentPicker state={attachments} disabled={busy} />}
          <ModelPicker
            workspaceId={workspaceId}
            agent={agent}
            value={model ?? live?.current ?? null}
            live={live}
            reasoning={reasoning}
            onSelect={onModel}
            onReasoningChange={onReasoning}
            compact
          />
          <PermissionPicker agent={agent} value={permission} onChange={onPermission} />
          {otherOptions.map((option) => (
            <OptionPicker key={option.id} option={option} disabled={optionsDisabled} onChange={(value) => onOption(option.id, value)} />
          ))}
        </div>
        <div className="flex items-center gap-1.5">
          <ContextMeter context={controls.context} />
          {busy && hasDraft && <ActionButton key="queue" mode="send" label="Queue (Enter)" loading={sending} disabled={!canSend} onClick={onSend} />}
          <ActionButton key="primary" mode={busy ? "stop" : "send"} label={busy ? "Stop (Esc)" : "Send (Enter)"}
            loading={!busy && sending} disabled={!busy && !canSend} onClick={busy ? onStop : onSend} />
        </div>
      </div>
    </div>
  );
}
