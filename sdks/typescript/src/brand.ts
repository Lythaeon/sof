declare const brandSymbol: unique symbol;

export type Brand<T, Name extends string> = T & {
  readonly [brandSymbol]: Name;
};

export function brand<T, Name extends string>(value: T): Brand<T, Name> {
  return value as Brand<T, Name>;
}
