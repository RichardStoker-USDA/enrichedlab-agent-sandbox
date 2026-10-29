export type ActionScope = {
  principal: string | null;
  revision: number;
  taskId: string;
};

export function matchesActionScope(current: ActionScope | null, owner: ActionScope, bindTask = false): boolean {
  return current?.principal === owner.principal && current.revision === owner.revision &&
    (!bindTask || current.taskId === owner.taskId);
}

// A response belongs to the account and navigation state that requested it.
export async function performAction<T>(
  current: () => ActionScope | null,
  request: () => Promise<T>,
  accept: (value: T) => void,
  bindTask = false,
): Promise<boolean> {
  const owner = current();
  if (!owner?.principal) return false;
  const matches = () => matchesActionScope(current(), owner, bindTask);
  let value: T;
  try {
    value = await request();
  } catch (error) {
    if (matches()) throw error;
    return false;
  }
  if (!matches()) return false;
  accept(value);
  return true;
}
