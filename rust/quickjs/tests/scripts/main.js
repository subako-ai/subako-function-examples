// Imports a module next to this file and reads data beside it.
const { total } = await import("./lib/sum.js");
const numbers = fs.readText("numbers.txt").trim().split("\n").map(Number);
console.log(`args: ${args.join(" ")}`);
return { total: total(numbers) };
