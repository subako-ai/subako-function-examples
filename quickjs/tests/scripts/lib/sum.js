import { add } from "./add.js";

export function total(numbers) {
  return numbers.reduce(add, 0);
}
