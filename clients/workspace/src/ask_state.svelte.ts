// The question being asked, shared by home's ask field, the bar's New question and the setup sheet. It lives for this
// visit only: a draft question is research content, so it is never kept in the browser or put in an address.
export const askDraft = $state<{ question: string; lab: string; open: boolean }>({ question: "", lab: "", open: false });
