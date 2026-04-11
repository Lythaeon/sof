export enum ResultTag {
  Ok = 1,
  Err = 2,
}

export type Ok<T> = {
  readonly tag: ResultTag.Ok;
  readonly value: T;
};

export type Err<E> = {
  readonly tag: ResultTag.Err;
  readonly error: E;
};

export type Result<T, E> = Ok<T> | Err<E>;

export function ok<T>(value: T): Ok<T> {
  return { tag: ResultTag.Ok, value };
}

export function err<E>(error: E): Err<E> {
  return { tag: ResultTag.Err, error };
}

export function isOk<T, E>(result: Result<T, E>): result is Ok<T> {
  return result.tag === ResultTag.Ok;
}

export function isErr<T, E>(result: Result<T, E>): result is Err<E> {
  return result.tag === ResultTag.Err;
}
