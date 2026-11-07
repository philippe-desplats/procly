import { expect } from "@wdio/globals";

import { procly, stopDaemon, writeScene, type Scene } from "../environment";
import { OVERVIEW, showingText } from "../window";

describe("a Service started with the CLI", () => {
  let scene: Scene;

  before(async () => {
    scene = await writeScene("web-app", "web");
  });

  after(async () => {
    await stopDaemon();
  });

  it("appears on the Overview with its port", async () => {
    const up = await procly(["up", "--json", scene.service], scene.path);
    expect(up.code).toBe(0);

    await showingText(OVERVIEW, [scene.service, String(scene.port)]);
  });
});
