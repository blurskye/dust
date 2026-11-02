/** A friendly greeting for `name`. */
export function hello(name: string): string {
  return `Hello, ${name}!`;
}

/** The same greeting, louder. */
export function shout(name: string): string {
  return hello(name).toUpperCase();
}
