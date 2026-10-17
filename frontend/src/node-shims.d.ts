/**
 * Minimal ambient declarations for the Node built-ins the tests use.
 *
 * The package deliberately has no `@types/node` dependency (typescript is the
 * only devDependency), so the handful of `node:test` / `node:assert` shapes
 * the test files touch are declared here. Runtime behaviour is Node's own.
 */
declare module "node:test" {
  export interface TestContext {
    readonly name: string;
    diagnostic(message: string): void;
    skip(message?: string): void;
    test(name: string, fn: TestFn): Promise<void>;
  }
  export type TestFn = (t: TestContext) => void | Promise<void>;
  export function test(name: string, fn: TestFn): Promise<void>;
  export function describe(name: string, fn: () => void): void;
  export function it(name: string, fn: TestFn): void;
  export function before(fn: () => void | Promise<void>): void;
  export function after(fn: () => void | Promise<void>): void;
  export function beforeEach(fn: () => void | Promise<void>): void;
  export function afterEach(fn: () => void | Promise<void>): void;
}

declare module "node:assert/strict" {
  type ErrorExpectation = RegExp | ((error: unknown) => boolean) | object;
  interface StrictAssert {
    (value: unknown, message?: string): asserts value;
    ok(value: unknown, message?: string): asserts value;
    equal(actual: unknown, expected: unknown, message?: string): void;
    notEqual(actual: unknown, expected: unknown, message?: string): void;
    deepEqual(actual: unknown, expected: unknown, message?: string): void;
    notDeepEqual(actual: unknown, expected: unknown, message?: string): void;
    throws(fn: () => unknown, expected?: ErrorExpectation, message?: string): void;
    rejects(
      promise: Promise<unknown> | (() => Promise<unknown>),
      expected?: ErrorExpectation,
      message?: string,
    ): Promise<void>;
    match(value: string, pattern: RegExp, message?: string): void;
    fail(message?: string): never;
  }
  const assert: StrictAssert;
  export default assert;
}
