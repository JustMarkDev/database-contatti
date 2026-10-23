import { defineTool, type ExtensionAPI } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

export default function register(pi: ExtensionAPI) {
  pi.registerTool(
    defineTool({
      name: "emit_batch",
      label: "Emit validated classification batch",
      description: "Submit the complete JSON result for the supplied contact batch exactly once.",
      parameters: Type.Object(
        {
          results: Type.Array(Type.Record(Type.String(), Type.Unknown())),
        },
        { additionalProperties: false },
      ),
      async execute(_toolCallId, params) {
        return {
          content: [{ type: "text", text: "Batch received." }],
          details: params,
          terminate: true,
        };
      },
    }),
  );
}
