// Ask first (Glass Box rule 1): a project starts from the question. Its title is drawn from the question and stays
// editable in the setup sheet; nothing here reads or writes research content beyond the text the person typed.

const MAX = 72;
const loose = new Set(["a", "an", "and", "as", "at", "by", "for", "from", "in", "of", "on", "or", "the", "to", "with"]);

/** A short title from a question: its first sentence, before a first comma when that part already says enough, cut at
 *  a word boundary within 72 characters, without a dangling small word or end mark. */
export function titleFrom(question: string): string {
  const text = question.replace(/\s+/g, " ").trim();
  if (!text) return "";
  let title = (/^(.+?[.?!])(\s|$)/.exec(text)?.[1] ?? text).replace(/[.?!]+$/, "");
  const comma = title.indexOf(",");
  if (comma >= 24) title = title.slice(0, comma);
  if (title.length > MAX) {
    const cut = title.slice(0, MAX + 1);
    title = cut.slice(0, Math.max(cut.lastIndexOf(" "), 1));
  }
  const words = title.replace(/[,;:\s]+$/, "").split(" ");
  while (words.length > 1 && loose.has(words[words.length - 1].toLowerCase())) words.pop();
  title = words.join(" ").replace(/[,;:]+$/, "");
  return title.charAt(0).toUpperCase() + title.slice(1);
}

/** The question as the project will save it: trimmed, with its own wording kept. */
export const questionText = (question: string) => question.trim();
