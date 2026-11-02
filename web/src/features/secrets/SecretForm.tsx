import {
  ActionIcon,
  Alert,
  Button,
  Card,
  Group,
  PasswordInput,
  Select,
  SimpleGrid,
  Stack,
  TagsInput,
  Text,
  TextInput,
  Textarea,
  Title,
  Tooltip,
} from '@mantine/core';
import { type UseFormReturnType, useForm } from '@mantine/form';
import { IconPlus, IconTrash } from '@tabler/icons-react';
import type { ReactNode } from 'react';

import {
  type SecretValues,
  newField,
  newFile,
  newInject,
  newSign,
  signFields,
  validateSecretName,
} from './secret';

type Form = UseFormReturnType<SecretValues>;

const required = (label: string) => (value: string) =>
  value.trim() === '' ? `Enter ${label}` : null;
const someHosts = (hosts: string[]) => (hosts.length > 0 ? null : 'Add at least one host');

// 一覧の項目の枠。右上に消すボタンを置く。
function Item({
  label,
  onRemove,
  removable = true,
  children,
}: {
  label: string;
  onRemove: () => void;
  removable?: boolean;
  children: ReactNode;
}) {
  return (
    <Card withBorder padding="sm">
      <Stack gap="xs">
        <Group justify="space-between" wrap="nowrap">
          <Text size="sm" fw={500}>
            {label}
          </Text>
          {removable && (
            <Tooltip label="Remove">
              {/* フォームの中のボタンは type="button" にする。 */}
              <ActionIcon
                type="button"
                variant="subtle"
                color="red"
                aria-label={`Remove ${label}`}
                onClick={onRemove}
              >
                <IconTrash size={16} />
              </ActionIcon>
            </Tooltip>
          )}
        </Group>
        {children}
      </Stack>
    </Card>
  );
}

function Section({
  title,
  description,
  addLabel,
  onAdd,
  children,
}: {
  title: string;
  description: ReactNode;
  addLabel: string;
  onAdd: () => void;
  children: ReactNode;
}) {
  return (
    <Stack gap="xs">
      <Title order={5}>{title}</Title>
      <Text size="xs" c="dimmed">
        {description}
      </Text>
      {children}
      <Group>
        <Button
          variant="light"
          size="compact-sm"
          leftSection={<IconPlus size={14} />}
          onClick={onAdd}
        >
          {addLabel}
        </Button>
      </Group>
    </Stack>
  );
}

function Fields({ form }: { form: Form }) {
  const fields = form.values.fields;
  return (
    <Section
      title="Fields"
      description={
        <>
          Each field is one value. The VM only sees its placeholder, which the proxy swaps for the
          real value. Most secrets have a single field named <code>value</code>.
        </>
      }
      addLabel="Add field"
      onAdd={() => form.insertListItem('fields', newField())}
    >
      {fields.map((field, index) => (
        <Item
          key={field.key}
          label={`Field ${field.name || index + 1}`}
          removable={fields.length > 1}
          onRemove={() => form.removeListItem('fields', index)}
        >
          <SimpleGrid cols={{ base: 1, xs: 2 }}>
            <TextInput
              label="Field name"
              autoComplete="off"
              {...form.getInputProps(`fields.${index}.name`)}
            />
            <TextInput
              label="Placeholder"
              placeholder="{id}"
              description="{id} becomes a random string per workspace."
              {...form.getInputProps(`fields.${index}.placeholder`)}
            />
          </SimpleGrid>
          <TagsInput
            label="Environment variables"
            placeholder="ANTHROPIC_API_KEY"
            splitChars={[',', ' ']}
            {...form.getInputProps(`fields.${index}.env`)}
          />
          <PasswordInput
            label={`Value of ${field.name || `field ${index + 1}`}`}
            autoComplete="new-password"
            {...form.getInputProps(`fields.${index}.value`)}
          />
        </Item>
      ))}
    </Section>
  );
}

function Inject({ form, fieldNames }: { form: Form; fieldNames: string[] }) {
  return (
    <Section
      title="Header rules"
      description="Swap the placeholder in a request header for the real value, only for these hosts."
      addLabel="Add header rule"
      onAdd={() => form.insertListItem('inject', newInject(fieldNames[0] ?? 'value'))}
    >
      {form.values.inject.map((rule, index) => (
        <Item
          key={rule.key}
          label={`Header rule ${index + 1}`}
          onRemove={() => form.removeListItem('inject', index)}
        >
          <TagsInput
            label={`Hosts of header rule ${index + 1}`}
            placeholder="api.example.com, *.example.com"
            splitChars={[',', ' ']}
            {...form.getInputProps(`inject.${index}.hosts`)}
          />
          <SimpleGrid cols={{ base: 1, xs: 2 }}>
            <Select
              label="Field"
              allowDeselect={false}
              data={fieldNames}
              {...form.getInputProps(`inject.${index}.field`)}
            />
            <TextInput label="Header" {...form.getInputProps(`inject.${index}.header`)} />
            <Select
              label="Format"
              allowDeselect={false}
              data={[
                { value: 'replace', label: 'Replace the placeholder' },
                { value: 'basic-password', label: 'Basic auth password' },
              ]}
              {...form.getInputProps(`inject.${index}.format`)}
            />
            {rule.format === 'basic-password' && (
              <Select
                label="User name field"
                placeholder="None"
                clearable
                data={fieldNames.filter((name) => name !== rule.field)}
                {...form.getInputProps(`inject.${index}.usernameField`)}
                value={rule.usernameField || null}
                onChange={(value) =>
                  form.setFieldValue(`inject.${index}.usernameField`, value ?? '')
                }
              />
            )}
          </SimpleGrid>
        </Item>
      ))}
    </Section>
  );
}

function Sign({ form }: { form: Form }) {
  return (
    <Section
      title="Signing rules"
      description="Re-sign requests made with the placeholder keys, for these hosts."
      addLabel="Add signing rule"
      onAdd={() => form.insertListItem('sign', newSign())}
    >
      {form.values.sign.map((rule, index) => {
        const { required: needs, optional } = signFields[rule.scheme];
        return (
          <Item
            key={rule.key}
            label={`Signing rule ${index + 1}`}
            onRemove={() => form.removeListItem('sign', index)}
          >
            <Select
              label="Scheme"
              allowDeselect={false}
              data={[{ value: 'aws-sigv4', label: 'AWS Signature V4' }]}
              {...form.getInputProps(`sign.${index}.scheme`)}
            />
            <Text size="xs" c="dimmed">
              Uses the fields {needs.join(', ')}
              {optional.length > 0 && ` (and ${optional.join(', ')} if defined)`}.
            </Text>
            <TagsInput
              label={`Hosts of signing rule ${index + 1}`}
              placeholder="*.amazonaws.com"
              splitChars={[',', ' ']}
              {...form.getInputProps(`sign.${index}.hosts`)}
            />
          </Item>
        );
      })}
    </Section>
  );
}

function Files({ form }: { form: Form }) {
  return (
    <Section
      title="Files"
      description={
        <>
          Files placed in the VM for tools that do not read environment variables. Write{' '}
          <code>{'{field}'}</code> in the content for a field&apos;s placeholder.
        </>
      }
      addLabel="Add file"
      onAdd={() => form.insertListItem('files', newFile())}
    >
      {form.values.files.map((file, index) => (
        <Item
          key={file.key}
          label={`File ${index + 1}`}
          onRemove={() => form.removeListItem('files', index)}
        >
          <TextInput
            label="Path"
            placeholder="/etc/apt/auth.conf.d/example.conf"
            {...form.getInputProps(`files.${index}.path`)}
          />
          <Textarea
            label="Content"
            autosize
            minRows={2}
            styles={{ input: { fontFamily: 'var(--mantine-font-family-monospace)' } }}
            {...form.getInputProps(`files.${index}.content`)}
          />
          <SimpleGrid cols={2}>
            <Select
              label="Owner"
              allowDeselect={false}
              data={[
                { value: 'root', label: 'root' },
                { value: 'user', label: 'user' },
              ]}
              {...form.getInputProps(`files.${index}.owner`)}
            />
            <TextInput
              label="Mode"
              placeholder="0644"
              {...form.getInputProps(`files.${index}.mode`)}
            />
          </SimpleGrid>
        </Item>
      ))}
    </Section>
  );
}

// シークレットの定義と値のフォーム。新しいシークレットと、定義を変えるときに使う。定義を変えるときも、
// すべての欄の値を入れ直す (画面からは、登録してある値を残して定義だけを変えられない)。
export function SecretForm({
  initial,
  editing,
  saving,
  onSubmit,
  onCancel,
}: {
  initial: SecretValues;
  // 登録してあるものの定義を変える (名前は変えられない)。
  editing: boolean;
  saving: boolean;
  onSubmit: (values: SecretValues, form: Form) => void;
  onCancel: () => void;
}) {
  const form = useForm<SecretValues>({
    initialValues: initial,
    validate: {
      name: (name) => (editing ? null : validateSecretName(name)),
      fields: {
        name: (name) => validateSecretName(name),
        value: required('the value'),
      },
      inject: { hosts: someHosts, header: required('a header') },
      sign: { hosts: someHosts },
      files: { path: (path) => (path.startsWith('/') ? null : 'Use an absolute path') },
    },
  });
  const fieldNames = form.values.fields.map((field) => field.name).filter((name) => name !== '');

  return (
    <form onSubmit={form.onSubmit((values) => onSubmit(values, form))}>
      <Stack>
        {editing ? (
          <Alert color="blue">
            Changing the definition replaces the stored values too, so enter every value again.
          </Alert>
        ) : (
          <TextInput
            label="Name"
            description="Lowercase letters, digits and _."
            autoComplete="off"
            data-autofocus
            {...form.getInputProps('name')}
          />
        )}
        <Fields form={form} />
        <Inject form={form} fieldNames={fieldNames} />
        <Sign form={form} />
        <Files form={form} />
        <Group justify="flex-end" gap="xs">
          <Button variant="default" onClick={onCancel} disabled={saving}>
            Cancel
          </Button>
          <Button type="submit" loading={saving}>
            Save
          </Button>
        </Group>
      </Stack>
    </form>
  );
}
