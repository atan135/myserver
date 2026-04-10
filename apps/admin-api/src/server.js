import { createApp } from "./app.js";

const { app, config } = await createApp();

app.listen(config.port, config.host, () => {
  console.log(`admin-api listening on ${config.host}:${config.port}`);
});
