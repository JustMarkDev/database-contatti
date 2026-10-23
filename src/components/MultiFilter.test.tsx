import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it } from "vite-plus/test";
import { MultiFilter } from "./MultiFilter";

const trigger = (include: string[], exclude: string[]) => {
  render(<MultiFilter label="Regione" value={{ include, exclude }} onChange={() => {}} />);
  return screen.getByRole("button");
};

describe("MultiFilter chip", () => {
  afterEach(cleanup);

  it("shows only the label when nothing is selected", () => {
    expect(trigger([], [])).toHaveTextContent(/^Regione$/);
  });

  it("summarises only the non-empty include/exclude counts", () => {
    expect(trigger(["Lombardia", "Piemonte"], [])).toHaveTextContent("Regione2 inclusi");
  });

  it("lists both counts when both are set", () => {
    expect(trigger(["Lombardia"], ["Lazio"])).toHaveTextContent("Regione1 incluso, 1 escluso");
  });
});

describe("MultiFilter draft", () => {
  afterEach(cleanup);

  it("commits once on close, not on every toggle", async () => {
    const calls: unknown[] = [];
    render(
      <MultiFilter
        label="Regione"
        options={[
          { value: "Lazio", count: 1 },
          { value: "Lombardia", count: 2 },
        ]}
        onChange={(next) => calls.push(next)}
      />,
    );
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /Regione/ }));
    await user.click(await screen.findByText("Lazio"));
    await user.click(screen.getByText("Lombardia"));
    await user.click(screen.getByText("Lombardia"));
    expect(calls).toHaveLength(0);
    await user.keyboard("{Escape}");
    expect(calls).toEqual([{ include: ["Lazio"], exclude: ["Lombardia"] }]);
  });
});
