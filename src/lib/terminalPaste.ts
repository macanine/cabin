// Pasting into a terminal (components/Terminal.tsx). The agents run in the
// machine and cannot see the Mac's clipboard, so a pasted image is saved in
// the machine and its path is typed instead: Claude Code turns a pasted
// image path into an attachment, and the other agents read the file.

/** Image types the machine accepts (core `terminal_paste_image`). */
export const PASTE_IMAGE_TYPES = ["image/png", "image/jpeg", "image/gif", "image/webp"] as const;

/** Largest pasted image, like the guest's limit. */
export const PASTE_IMAGE_MAX = 20 * 1024 * 1024;

interface ItemLike { kind: string; type: string; getAsFile(): Blob | null }

/** The first image in a paste, or null (then the paste is text as usual). */
export function pastedImage(items: ArrayLike<ItemLike> | null | undefined): Blob | null {
  for (const item of Array.from(items ?? [])) {
    if (item.kind === "file" && (PASTE_IMAGE_TYPES as readonly string[]).includes(item.type)) {
      const file = item.getAsFile();
      if (file) return file;
    }
  }
  return null;
}

/** A path as typed into a shell or an agent prompt: quoted only if needed. */
export function typedPath(path: string): string {
  return /^[\w@%+=:,./-]+$/.test(path) ? path : `'${path.replace(/'/g, `'\\''`)}'`;
}

/** Base64 of a blob, without the data-URL prefix. */
export async function blobBase64(blob: Blob): Promise<string> {
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = "";
  for (let i = 0; i < bytes.length; i += 0x8000) binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(binary);
}
