import { Field, Select } from "@/components/ui";
import { useWorkspace } from "@/lib/workspace";

export function ProjectDot({ color }: { color: string }) {
  return <span className="project-dot" style={{ background: color }} aria-hidden="true" />;
}

/** Project picker for create forms. Hidden while the workspace has one project. */
export function ProjectField({ value, onChange, error }: { value: string; onChange: (id: string) => void; error?: string }) {
  const { projects } = useWorkspace();
  if (projects.length < 2) return null;
  return (
    <Field label="Project" error={error}>
      <Select
        aria-label="Project"
        value={value}
        options={projects.map((project) => ({ value: project.id, label: project.name }))}
        onChange={onChange}
      />
    </Field>
  );
}

/** Names a record's project in lists, only while "All projects" is selected. */
export function ProjectTag({ id }: { id: string }) {
  const { projects, project } = useWorkspace();
  const owner = projects.find((candidate) => candidate.id === id);
  if (project !== "all" || projects.length < 2 || !owner) return null;
  return (
    <span className="project-tag" title={`Project ${owner.name}`}>
      <ProjectDot color={owner.color} />
      <span className="truncate">{owner.name}</span>
    </span>
  );
}
