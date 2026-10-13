import { useState } from "react";
import MainView from "./MainView";
import DetailView from "./DetailView";

type View = { name: "main" } | { name: "detail"; dir: string };

export default function App() {
  const [view, setView] = useState<View>({ name: "main" });

  // MainView stays mounted so an ongoing recording keeps its live state while viewing details.
  return (
    <>
      <div hidden={view.name !== "main"}>
        <MainView onOpen={(dir) => setView({ name: "detail", dir })} />
      </div>
      {view.name === "detail" && <DetailView dir={view.dir} onBack={() => setView({ name: "main" })} />}
    </>
  );
}
