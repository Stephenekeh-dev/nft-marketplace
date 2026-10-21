require('ts-node').register({ transpileOnly: true });

const Mocha = require('mocha');
const fs = require('fs');
const path = require('path');

const mocha = new Mocha({ timeout: 1000000 });
const dir = path.join(__dirname, 'tests');
fs.readdirSync(dir)
  .filter((f) => f.endsWith('.ts'))
  .forEach((f) => mocha.addFile(path.join(dir, f)));

mocha.run((failures) => {
  process.exitCode = failures ? 1 : 0;
});
