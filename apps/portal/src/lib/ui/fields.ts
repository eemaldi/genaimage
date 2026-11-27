export type FieldProps = {
  id?: string;
  label: string;
  hint?: string | Array<string | null | undefined>;
  error?: string;
};

export type NativeFieldProps = {
  name?: string;
  disabled?: boolean;
  required?: boolean;
};
