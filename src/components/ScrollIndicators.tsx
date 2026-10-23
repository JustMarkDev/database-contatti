import { useCallback, useEffect, useRef } from "react";

export function useScrollActivity() {
  const ref = useRef<HTMLDivElement>(null);
  const verticalRef = useRef<HTMLSpanElement>(null);
  const horizontalRef = useRef<HTMLSpanElement>(null);
  const timeout = useRef<number | null>(null);
  const updateIndicators = useCallback(() => {
    const element = ref.current;
    if (!element) return;
    const verticalSize =
      element.scrollHeight > element.clientHeight
        ? Math.max(24, element.clientHeight ** 2 / element.scrollHeight)
        : 0;
    const horizontalSize =
      element.scrollWidth > element.clientWidth
        ? Math.max(24, element.clientWidth ** 2 / element.scrollWidth)
        : 0;
    if (verticalRef.current) {
      verticalRef.current.style.height = `${verticalSize}px`;
      verticalRef.current.style.transform = `translateY(${verticalSize ? (element.scrollTop / (element.scrollHeight - element.clientHeight)) * (element.clientHeight - verticalSize) : 0}px)`;
      verticalRef.current.style.display = verticalSize ? "block" : "none";
    }
    if (horizontalRef.current) {
      horizontalRef.current.style.width = `${horizontalSize}px`;
      horizontalRef.current.style.transform = `translateX(${horizontalSize ? (element.scrollLeft / (element.scrollWidth - element.clientWidth)) * (element.clientWidth - horizontalSize) : 0}px)`;
      horizontalRef.current.style.display = horizontalSize ? "block" : "none";
    }
  }, []);
  const onScroll = useCallback(() => {
    updateIndicators();
    verticalRef.current?.classList.add("visible");
    horizontalRef.current?.classList.add("visible");
    if (timeout.current !== null) window.clearTimeout(timeout.current);
    timeout.current = window.setTimeout(() => {
      verticalRef.current?.classList.remove("visible");
      horizontalRef.current?.classList.remove("visible");
    }, 650);
  }, [updateIndicators]);
  useEffect(() => {
    updateIndicators();
    const observer = new ResizeObserver(updateIndicators);
    if (ref.current) observer.observe(ref.current);
    return () => observer.disconnect();
  }, [updateIndicators]);
  useEffect(
    () => () => {
      if (timeout.current !== null) window.clearTimeout(timeout.current);
    },
    [],
  );
  return { ref, verticalRef, horizontalRef, onScroll };
}

export function ScrollIndicators({ activity }: { activity: ReturnType<typeof useScrollActivity> }) {
  return (
    <>
      <span ref={activity.verticalRef} className="overlay-scrollbar vertical" />
      <span ref={activity.horizontalRef} className="overlay-scrollbar horizontal" />
    </>
  );
}
