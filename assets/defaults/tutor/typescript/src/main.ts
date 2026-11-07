import { hello, shout } from "./greet";

const name = "Dust";
const message = hello(name);
console.log(message);
const unused = 42;
for (let i = 0; i < 3; i++) {
  console.log(shout(name));
}
